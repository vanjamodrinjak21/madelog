package httpapi

import (
	"errors"
	"fmt"
	"net/http"
	"strings"
	"time"

	"github.com/go-chi/chi/v5"
	"github.com/google/uuid"

	"github.com/madelog/server/internal/sign"
	"github.com/madelog/server/internal/slug"
	"github.com/madelog/server/internal/store"
)

// slugAttempts is how many times we retry a random slug before giving up.
// With 20 bits in the suffix on top of the word pair, one collision is already
// unlikely; ten in a row means something is wrong.
const slugAttempts = 10

type publishRequest struct {
	DeviceID    string `json:"device_id"`
	Slug        string `json:"slug"`
	HTML        string `json:"html"`
	Root        string `json:"root"`
	FirstTS     string `json:"first_ts"`
	LastTS      string `json:"last_ts"`
	PublishedTS string `json:"published_ts"`
	Signature   string `json:"signature"`
}

type publishResponse struct {
	Slug       string    `json:"slug"`
	URL        string    `json:"url"`
	VerifyURL  string    `json:"verify_url"`
	ReceivedAt time.Time `json:"received_at"`
}

// handlePublishPage stores a signed page. See server/README.md for the exact
// byte string the signature covers.
func (a *api) handlePublishPage(w http.ResponseWriter, r *http.Request) {
	// The envelope allowance is generous because JSON escaping can inflate
	// html well past its raw size; the precise 2 MiB rule is applied to the
	// decoded html below.
	limit := 4*a.cfg.MaxHTMLBytes + 64<<10

	var req publishRequest
	if tooLarge, err := decodeJSON(w, r, limit, &req); err != nil {
		if tooLarge {
			writeError(w, r, http.StatusRequestEntityTooLarge, "too_large", "request body is too large")
			return
		}
		writeError(w, r, http.StatusBadRequest, "bad_request", "body must be a JSON object: "+err.Error())
		return
	}

	if int64(len(req.HTML)) > a.cfg.MaxHTMLBytes {
		writeError(w, r, http.StatusRequestEntityTooLarge, "html_too_large", "html must be at most 2 MB")
		return
	}
	if req.HTML == "" {
		writeError(w, r, http.StatusBadRequest, "bad_request", "html is required")
		return
	}
	if !sign.ValidRootHex(req.Root) {
		writeError(w, r, http.StatusBadRequest, "bad_root", "root must be 64 lowercase hex characters")
		return
	}
	firstTS, err := parseUTC(req.FirstTS)
	if err != nil {
		writeError(w, r, http.StatusBadRequest, "bad_timestamp", "first_ts must be RFC 3339 in UTC")
		return
	}
	lastTS, err := parseUTC(req.LastTS)
	if err != nil {
		writeError(w, r, http.StatusBadRequest, "bad_timestamp", "last_ts must be RFC 3339 in UTC")
		return
	}
	publishedTS, err := parseUTC(req.PublishedTS)
	if err != nil {
		writeError(w, r, http.StatusBadRequest, "bad_timestamp", "published_ts must be RFC 3339 in UTC")
		return
	}
	if req.Slug != "" && !slug.Valid(req.Slug) {
		writeError(w, r, http.StatusBadRequest, "bad_slug", "slug must be 3-64 lowercase alphanumeric characters and single hyphens")
		return
	}
	deviceID, err := uuid.Parse(req.DeviceID)
	if err != nil {
		writeError(w, r, http.StatusBadRequest, "bad_device_id", "device_id must be a UUID")
		return
	}
	if _, err := sign.ParseSignatureHex(req.Signature); err != nil {
		writeError(w, r, http.StatusBadRequest, "bad_signature", "signature must be a hex-encoded 64-byte ed25519 signature")
		return
	}

	dev, err := a.store.Device(r.Context(), deviceID)
	if errors.Is(err, store.ErrNotFound) {
		writeError(w, r, http.StatusNotFound, "unknown_device", "register the device with POST /v1/devices first")
		return
	} else if err != nil {
		a.internalError(w, r, "loading device", err)
		return
	}

	// The signature covers the slug exactly as sent, which is the empty string
	// when the client is asking us to mint one.
	msg := sign.PublishMessage(req.Root, req.PublishedTS, req.Slug)
	if err := sign.VerifyHex(dev.PublicKey, msg, req.Signature); err != nil {
		writeError(w, r, http.StatusUnauthorized, "bad_signature", "signature does not verify for this device")
		return
	}

	pageSlug := req.Slug
	if pageSlug == "" {
		pageSlug, err = a.mintSlug(r)
		if err != nil {
			a.internalError(w, r, "minting slug", err)
			return
		}
	} else {
		existing, err := a.store.Page(r.Context(), pageSlug)
		switch {
		case err == nil && existing.DeviceID != deviceID:
			writeError(w, r, http.StatusForbidden, "slug_taken", "this slug belongs to another device")
			return
		case err != nil && !errors.Is(err, store.ErrNotFound):
			a.internalError(w, r, "loading page", err)
			return
		}
	}

	page, err := a.store.PublishPage(r.Context(), store.PublishPageParams{
		Slug:        pageSlug,
		DeviceID:    deviceID,
		HTML:        req.HTML,
		Root:        req.Root,
		FirstTS:     firstTS,
		LastTS:      lastTS,
		PublishedTS: publishedTS,
	})
	if err != nil {
		a.internalError(w, r, "publishing page", err)
		return
	}

	writeJSON(w, r, http.StatusOK, publishResponse{
		Slug:       page.Slug,
		URL:        a.pageURL(page.Slug),
		VerifyURL:  a.verifyURL(page.Slug),
		ReceivedAt: page.ReceivedAt.UTC(),
	})
}

// mintSlug returns a slug nothing has used yet. Deleted pages keep their row,
// so their slugs stay reserved and are never handed to a second person.
func (a *api) mintSlug(r *http.Request) (string, error) {
	for i := 0; i < slugAttempts; i++ {
		candidate, err := slug.Generate()
		if err != nil {
			return "", err
		}
		_, err = a.store.Page(r.Context(), candidate)
		if errors.Is(err, store.ErrNotFound) {
			return candidate, nil
		}
		if err != nil {
			return "", err
		}
	}
	return "", fmt.Errorf("could not find a free slug in %d attempts", slugAttempts)
}

// handleServePage serves the stored html. Deleted pages are gone for good as
// far as readers are concerned. `/p/{slug}.pdf` is the same page as a PDF;
// a slug can never contain a dot, so the suffix is unambiguous.
func (a *api) handleServePage(w http.ResponseWriter, r *http.Request) {
	s := chi.URLParam(r, "slug")
	if base, ok := strings.CutSuffix(s, ".pdf"); ok {
		if !slug.Valid(base) {
			http.NotFound(w, r)
			return
		}
		a.handleServePDF(w, r, base)
		return
	}
	if !slug.Valid(s) {
		http.NotFound(w, r)
		return
	}
	page, err := a.store.Page(r.Context(), s)
	if errors.Is(err, store.ErrNotFound) || (err == nil && page.Deleted()) {
		http.NotFound(w, r)
		return
	}
	if err != nil {
		a.internalError(w, r, "loading page", err)
		return
	}

	w.Header().Set("Content-Type", "text/html; charset=utf-8")
	w.Header().Set("Cache-Control", "public, max-age=60")
	w.Header().Set("X-Content-Type-Options", "nosniff")
	w.WriteHeader(http.StatusOK)
	_, _ = w.Write([]byte(page.HTML))
}

type verifyResponse struct {
	Slug       string    `json:"slug"`
	Root       string    `json:"root"`
	FirstTS    time.Time `json:"first_ts"`
	LastTS     time.Time `json:"last_ts"`
	ReceivedAt time.Time `json:"received_at"`
	DeviceID   string    `json:"device_id"`
}

// handleVerify returns the verification record behind the page badge. It never
// includes html.
func (a *api) handleVerify(w http.ResponseWriter, r *http.Request) {
	s := chi.URLParam(r, "slug")
	if !slug.Valid(s) {
		writeError(w, r, http.StatusNotFound, "not_found", "no such page")
		return
	}
	page, err := a.store.Page(r.Context(), s)
	if errors.Is(err, store.ErrNotFound) || (err == nil && page.Deleted()) {
		writeError(w, r, http.StatusNotFound, "not_found", "no such page")
		return
	}
	if err != nil {
		a.internalError(w, r, "loading page", err)
		return
	}
	writeJSON(w, r, http.StatusOK, verifyResponse{
		Slug:       page.Slug,
		Root:       page.Root,
		FirstTS:    page.FirstTS.UTC(),
		LastTS:     page.LastTS.UTC(),
		ReceivedAt: page.ReceivedAt.UTC(),
		DeviceID:   page.DeviceID.String(),
	})
}

type deleteRequest struct {
	DeviceID  string `json:"device_id"`
	Signature string `json:"signature"`
}

type deleteResponse struct {
	Slug    string `json:"slug"`
	Deleted bool   `json:"deleted"`
}

// handleDeletePage drops the html and keeps the root, so a page can disappear
// without the chain history disappearing with it. Idempotent.
func (a *api) handleDeletePage(w http.ResponseWriter, r *http.Request) {
	s := chi.URLParam(r, "slug")
	if !slug.Valid(s) {
		writeError(w, r, http.StatusNotFound, "not_found", "no such page")
		return
	}

	var req deleteRequest
	if tooLarge, err := decodeJSON(w, r, 4<<10, &req); err != nil {
		if tooLarge {
			writeError(w, r, http.StatusRequestEntityTooLarge, "too_large", "request body is too large")
			return
		}
		writeError(w, r, http.StatusBadRequest, "bad_request", "body must be a JSON object with device_id and signature")
		return
	}
	deviceID, err := uuid.Parse(req.DeviceID)
	if err != nil {
		writeError(w, r, http.StatusBadRequest, "bad_device_id", "device_id must be a UUID")
		return
	}
	if _, err := sign.ParseSignatureHex(req.Signature); err != nil {
		writeError(w, r, http.StatusBadRequest, "bad_signature", "signature must be a hex-encoded 64-byte ed25519 signature")
		return
	}

	dev, err := a.store.Device(r.Context(), deviceID)
	if errors.Is(err, store.ErrNotFound) {
		writeError(w, r, http.StatusNotFound, "unknown_device", "no such device")
		return
	} else if err != nil {
		a.internalError(w, r, "loading device", err)
		return
	}
	if err := sign.VerifyHex(dev.PublicKey, sign.DeleteMessage(s), req.Signature); err != nil {
		writeError(w, r, http.StatusUnauthorized, "bad_signature", "signature does not verify for this device")
		return
	}

	page, err := a.store.Page(r.Context(), s)
	if errors.Is(err, store.ErrNotFound) {
		writeError(w, r, http.StatusNotFound, "not_found", "no such page")
		return
	} else if err != nil {
		a.internalError(w, r, "loading page", err)
		return
	}
	if page.DeviceID != deviceID {
		writeError(w, r, http.StatusForbidden, "not_owner", "this page belongs to another device")
		return
	}

	if _, err := a.store.SoftDeletePage(r.Context(), s); err != nil {
		a.internalError(w, r, "deleting page", err)
		return
	}
	writeJSON(w, r, http.StatusOK, deleteResponse{Slug: s, Deleted: true})
}

// parseUTC accepts RFC 3339 timestamps whose offset is zero. The CLI always
// sends "Z"; "+00:00" is accepted because it is the same instant.
func parseUTC(s string) (time.Time, error) {
	t, err := time.Parse(time.RFC3339, s)
	if err != nil {
		return time.Time{}, err
	}
	if _, offset := t.Zone(); offset != 0 {
		return time.Time{}, errors.New("timestamp must be in UTC")
	}
	return t.UTC(), nil
}
