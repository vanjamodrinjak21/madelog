package httpapi_test

import (
	"bytes"
	"crypto/ed25519"
	"crypto/hmac"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"io"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	"github.com/madelog/server/internal/httpapi"
	"github.com/madelog/server/internal/sign"
	"github.com/madelog/server/internal/slug"
	"github.com/madelog/server/internal/store"
)

const (
	baseURL   = "https://madelog.example"
	lsSecret  = "whsec-test"
	rootHex   = "3d9f1c0a4b5e6f708192a3b4c5d6e7f80112233445566778899aabbccddeeff0"
	rootHexB  = "aa9f1c0a4b5e6f708192a3b4c5d6e7f80112233445566778899aabbccddeeff0"
	publishTS = "2026-09-17T14:05:00Z"
	firstTS   = "2026-09-03T09:12:00Z"
	lastTS    = "2026-09-17T14:02:00Z"
)

func quietLogger() *slog.Logger { return slog.New(slog.DiscardHandler) }

type harness struct {
	t   *testing.T
	mem *store.Memory
	srv http.Handler
}

func newHarness(t *testing.T) *harness {
	t.Helper()
	mem := store.NewMemory()
	h, err := httpapi.NewRouter(mem, httpapi.Config{
		PublicBaseURL:      baseURL,
		LemonSqueezySecret: lsSecret,
		Logger:             quietLogger(),
	})
	if err != nil {
		t.Fatal(err)
	}
	return &harness{t: t, mem: mem, srv: h}
}

func (h *harness) do(method, path string, body any, headers ...[2]string) *httptest.ResponseRecorder {
	h.t.Helper()
	var r io.Reader
	switch b := body.(type) {
	case nil:
		r = nil
	case string:
		r = strings.NewReader(b)
	case []byte:
		r = bytes.NewReader(b)
	default:
		raw, err := json.Marshal(b)
		if err != nil {
			h.t.Fatal(err)
		}
		r = bytes.NewReader(raw)
	}
	req := httptest.NewRequest(method, path, r)
	if body != nil {
		req.Header.Set("Content-Type", "application/json")
	}
	for _, kv := range headers {
		req.Header.Set(kv[0], kv[1])
	}
	rec := httptest.NewRecorder()
	h.srv.ServeHTTP(rec, req)
	return rec
}

func decode[T any](t *testing.T, rec *httptest.ResponseRecorder) T {
	t.Helper()
	var out T
	if err := json.Unmarshal(rec.Body.Bytes(), &out); err != nil {
		t.Fatalf("decoding %q: %v", rec.Body.String(), err)
	}
	return out
}

type device struct {
	pub  ed25519.PublicKey
	priv ed25519.PrivateKey
	id   string
}

func (h *harness) newDevice() device {
	h.t.Helper()
	pub, priv, err := ed25519.GenerateKey(nil)
	if err != nil {
		h.t.Fatal(err)
	}
	rec := h.do(http.MethodPost, "/v1/devices", map[string]string{"public_key": hex.EncodeToString(pub)})
	if rec.Code != http.StatusOK {
		h.t.Fatalf("POST /v1/devices = %d: %s", rec.Code, rec.Body.String())
	}
	out := decode[struct {
		DeviceID string `json:"device_id"`
	}](h.t, rec)
	if out.DeviceID == "" {
		h.t.Fatal("no device_id in response")
	}
	return device{pub: pub, priv: priv, id: out.DeviceID}
}

// publishBody builds a POST /v1/pages body signed with d's key. A nil slug
// pointer means "no slug field at all", which is how a first publish looks.
func publishBody(d device, slugValue *string, html, root string) map[string]any {
	s := ""
	body := map[string]any{
		"device_id":    d.id,
		"html":         html,
		"root":         root,
		"first_ts":     firstTS,
		"last_ts":      lastTS,
		"published_ts": publishTS,
	}
	if slugValue != nil {
		s = *slugValue
		body["slug"] = s
	}
	msg := sign.PublishMessage(root, publishTS, s)
	body["signature"] = hex.EncodeToString(ed25519.Sign(d.priv, msg))
	return body
}

func TestDevicesIdempotentOnPublicKey(t *testing.T) {
	h := newHarness(t)
	pub, _, _ := ed25519.GenerateKey(nil)
	hexKey := hex.EncodeToString(pub)

	first := decode[map[string]string](t, h.do(http.MethodPost, "/v1/devices", map[string]string{"public_key": hexKey}))
	second := decode[map[string]string](t, h.do(http.MethodPost, "/v1/devices", map[string]string{"public_key": hexKey}))
	if first["device_id"] != second["device_id"] {
		t.Fatalf("not idempotent: %s vs %s", first["device_id"], second["device_id"])
	}
}

func TestDevicesRejectsBadKeys(t *testing.T) {
	h := newHarness(t)
	for name, key := range map[string]string{
		"empty":     "",
		"not hex":   "zzzz",
		"too short": hex.EncodeToString(make([]byte, 31)),
		"too long":  hex.EncodeToString(make([]byte, 33)),
	} {
		rec := h.do(http.MethodPost, "/v1/devices", map[string]string{"public_key": key})
		if rec.Code != http.StatusBadRequest {
			t.Errorf("%s: got %d, want 400", name, rec.Code)
		}
	}
	if rec := h.do(http.MethodPost, "/v1/devices", "{not json"); rec.Code != http.StatusBadRequest {
		t.Errorf("malformed json: got %d, want 400", rec.Code)
	}
}

func TestPublishWithoutSlugGeneratesOne(t *testing.T) {
	h := newHarness(t)
	d := h.newDevice()

	rec := h.do(http.MethodPost, "/v1/pages", publishBody(d, nil, "<h1>hello</h1>", rootHex))
	if rec.Code != http.StatusOK {
		t.Fatalf("got %d: %s", rec.Code, rec.Body.String())
	}
	out := decode[struct {
		Slug       string    `json:"slug"`
		URL        string    `json:"url"`
		ReceivedAt time.Time `json:"received_at"`
	}](t, rec)

	if !slug.Valid(out.Slug) {
		t.Fatalf("generated slug %q is not valid", out.Slug)
	}
	if want := baseURL + "/p/" + out.Slug; out.URL != want {
		t.Fatalf("url = %q, want %q", out.URL, want)
	}
	if out.ReceivedAt.IsZero() {
		t.Fatal("received_at must be set by the server")
	}
	if time.Since(out.ReceivedAt) > time.Minute {
		t.Fatalf("received_at looks wrong: %v", out.ReceivedAt)
	}
}

func TestPublishWithExplicitSlug(t *testing.T) {
	h := newHarness(t)
	d := h.newDevice()
	s := "quiet-heron-k7m2"

	rec := h.do(http.MethodPost, "/v1/pages", publishBody(d, &s, "<h1>hello</h1>", rootHex))
	if rec.Code != http.StatusOK {
		t.Fatalf("got %d: %s", rec.Code, rec.Body.String())
	}
	if got := decode[map[string]any](t, rec)["slug"]; got != s {
		t.Fatalf("slug = %v, want %v", got, s)
	}
}

func TestRepublishOverwritesAndAppendsRoot(t *testing.T) {
	h := newHarness(t)
	d := h.newDevice()
	s := "quiet-heron-k7m2"

	if rec := h.do(http.MethodPost, "/v1/pages", publishBody(d, &s, "<h1>one</h1>", rootHex)); rec.Code != http.StatusOK {
		t.Fatalf("first publish: %d %s", rec.Code, rec.Body.String())
	}
	if rec := h.do(http.MethodPost, "/v1/pages", publishBody(d, &s, "<h1>two</h1>", rootHexB)); rec.Code != http.StatusOK {
		t.Fatalf("second publish: %d %s", rec.Code, rec.Body.String())
	}

	page := h.do(http.MethodGet, "/p/"+s, nil)
	if page.Body.String() != "<h1>two</h1>" {
		t.Fatalf("html not overwritten: %q", page.Body.String())
	}
	roots, err := h.mem.Roots(t.Context(), s)
	if err != nil {
		t.Fatal(err)
	}
	if len(roots) != 2 {
		t.Fatalf("got %d roots rows, want 2", len(roots))
	}
}

func TestPublishToSomeoneElsesSlugIs403(t *testing.T) {
	h := newHarness(t)
	owner := h.newDevice()
	intruder := h.newDevice()
	s := "quiet-heron-k7m2"

	if rec := h.do(http.MethodPost, "/v1/pages", publishBody(owner, &s, "<h1>mine</h1>", rootHex)); rec.Code != http.StatusOK {
		t.Fatalf("setup publish: %d %s", rec.Code, rec.Body.String())
	}
	rec := h.do(http.MethodPost, "/v1/pages", publishBody(intruder, &s, "<h1>yours</h1>", rootHexB))
	if rec.Code != http.StatusForbidden {
		t.Fatalf("got %d, want 403: %s", rec.Code, rec.Body.String())
	}
	if body := h.do(http.MethodGet, "/p/"+s, nil).Body.String(); body != "<h1>mine</h1>" {
		t.Fatalf("owner's html was changed: %q", body)
	}
}

func TestPublishRejectsBadSignature(t *testing.T) {
	h := newHarness(t)
	d := h.newDevice()
	other := h.newDevice()

	t.Run("signed by another device", func(t *testing.T) {
		body := publishBody(d, nil, "<h1>x</h1>", rootHex)
		msg := sign.PublishMessage(rootHex, publishTS, "")
		body["signature"] = hex.EncodeToString(ed25519.Sign(other.priv, msg))
		if rec := h.do(http.MethodPost, "/v1/pages", body); rec.Code != http.StatusUnauthorized {
			t.Fatalf("got %d, want 401: %s", rec.Code, rec.Body.String())
		}
	})

	t.Run("signature over a different root", func(t *testing.T) {
		body := publishBody(d, nil, "<h1>x</h1>", rootHex)
		body["root"] = rootHexB
		if rec := h.do(http.MethodPost, "/v1/pages", body); rec.Code != http.StatusUnauthorized {
			t.Fatalf("got %d, want 401", rec.Code)
		}
	})

	t.Run("signature over a different published_ts", func(t *testing.T) {
		body := publishBody(d, nil, "<h1>x</h1>", rootHex)
		body["published_ts"] = "2026-09-17T15:05:00Z"
		if rec := h.do(http.MethodPost, "/v1/pages", body); rec.Code != http.StatusUnauthorized {
			t.Fatalf("got %d, want 401", rec.Code)
		}
	})

	t.Run("slug added after signing over the empty slug", func(t *testing.T) {
		body := publishBody(d, nil, "<h1>x</h1>", rootHex)
		body["slug"] = "quiet-heron-k7m2"
		if rec := h.do(http.MethodPost, "/v1/pages", body); rec.Code != http.StatusUnauthorized {
			t.Fatalf("got %d, want 401", rec.Code)
		}
	})

	t.Run("malformed signature", func(t *testing.T) {
		body := publishBody(d, nil, "<h1>x</h1>", rootHex)
		body["signature"] = "nothex"
		if rec := h.do(http.MethodPost, "/v1/pages", body); rec.Code != http.StatusBadRequest {
			t.Fatalf("got %d, want 400", rec.Code)
		}
	})
}

func TestPublishValidatesFields(t *testing.T) {
	h := newHarness(t)
	d := h.newDevice()

	cases := map[string]func(m map[string]any){
		"empty html":        func(m map[string]any) { m["html"] = "" },
		"bad root":          func(m map[string]any) { m["root"] = "nope" },
		"uppercase root":    func(m map[string]any) { m["root"] = strings.ToUpper(rootHex) },
		"bad first_ts":      func(m map[string]any) { m["first_ts"] = "yesterday" },
		"non-utc published": func(m map[string]any) { m["published_ts"] = "2026-09-17T14:05:00+02:00" },
		"bad slug":          func(m map[string]any) { m["slug"] = "../../etc/passwd" },
		"missing device":    func(m map[string]any) { delete(m, "device_id") },
		"bad device uuid":   func(m map[string]any) { m["device_id"] = "not-a-uuid" },
	}
	for name, mutate := range cases {
		body := publishBody(d, nil, "<h1>x</h1>", rootHex)
		mutate(body)
		rec := h.do(http.MethodPost, "/v1/pages", body)
		if rec.Code != http.StatusBadRequest {
			t.Errorf("%s: got %d, want 400 (%s)", name, rec.Code, rec.Body.String())
		}
	}
}

func TestPublishUnknownDeviceIs404(t *testing.T) {
	h := newHarness(t)
	d := h.newDevice()
	body := publishBody(d, nil, "<h1>x</h1>", rootHex)
	body["device_id"] = "6ba7b810-9dad-11d1-80b4-00c04fd430c8"
	if rec := h.do(http.MethodPost, "/v1/pages", body); rec.Code != http.StatusNotFound {
		t.Fatalf("got %d, want 404: %s", rec.Code, rec.Body.String())
	}
}

func TestPublishRejectsHTMLOver2MB(t *testing.T) {
	h := newHarness(t)
	d := h.newDevice()

	big := strings.Repeat("a", 2*1024*1024+1)
	if rec := h.do(http.MethodPost, "/v1/pages", publishBody(d, nil, big, rootHex)); rec.Code != http.StatusRequestEntityTooLarge {
		t.Fatalf("got %d, want 413", rec.Code)
	}

	// Just under the limit is fine.
	ok := strings.Repeat("a", 2*1024*1024-1)
	if rec := h.do(http.MethodPost, "/v1/pages", publishBody(d, nil, ok, rootHex)); rec.Code != http.StatusOK {
		t.Fatalf("2MB-1 html: got %d, want 200: %s", rec.Code, rec.Body.String())
	}
}

func TestPublishRejectsAbsurdBody(t *testing.T) {
	h := newHarness(t)
	d := h.newDevice()
	body := publishBody(d, nil, "<h1>x</h1>", rootHex)
	raw, _ := json.Marshal(body)
	padded := append(raw[:len(raw)-1], []byte(`,"junk":"`+strings.Repeat("x", 12*1024*1024)+`"}`)...)
	if rec := h.do(http.MethodPost, "/v1/pages", padded); rec.Code != http.StatusRequestEntityTooLarge {
		t.Fatalf("got %d, want 413", rec.Code)
	}
}

func TestServePage(t *testing.T) {
	h := newHarness(t)
	d := h.newDevice()
	s := "quiet-heron-k7m2"
	if rec := h.do(http.MethodPost, "/v1/pages", publishBody(d, &s, "<!doctype html><h1>hi</h1>", rootHex)); rec.Code != http.StatusOK {
		t.Fatal(rec.Body.String())
	}

	rec := h.do(http.MethodGet, "/p/"+s, nil)
	if rec.Code != http.StatusOK {
		t.Fatalf("got %d", rec.Code)
	}
	if ct := rec.Header().Get("Content-Type"); ct != "text/html; charset=utf-8" {
		t.Fatalf("Content-Type = %q", ct)
	}
	if cc := rec.Header().Get("Cache-Control"); cc != "public, max-age=60" {
		t.Fatalf("Cache-Control = %q", cc)
	}
	if rec.Body.String() != "<!doctype html><h1>hi</h1>" {
		t.Fatalf("body = %q", rec.Body.String())
	}

	if rec := h.do(http.MethodGet, "/p/no-such-slug-aaaa", nil); rec.Code != http.StatusNotFound {
		t.Fatalf("unknown slug: got %d, want 404", rec.Code)
	}
}

// `curl -I <page url>` is the first thing anyone reaches for when a link looks
// broken, and uptime checkers poll /healthz with HEAD. Both must answer 200
// with the same headers as GET and no body.
func TestHeadIsRoutedLikeGet(t *testing.T) {
	h := newHarness(t)
	d := h.newDevice()
	s := "quiet-heron-k7m2"
	if rec := h.do(http.MethodPost, "/v1/pages", publishBody(d, &s, "<!doctype html><h1>hi</h1>", rootHex)); rec.Code != http.StatusOK {
		t.Fatal(rec.Body.String())
	}

	// A real server: net/http is what drops the body of a HEAD response.
	ts := httptest.NewServer(h.srv)
	defer ts.Close()

	for _, tc := range []struct {
		path        string
		contentType string
	}{
		{"/p/" + s, "text/html; charset=utf-8"},
		{"/healthz", "application/json; charset=utf-8"},
	} {
		resp, err := http.Head(ts.URL + tc.path)
		if err != nil {
			t.Fatalf("HEAD %s: %v", tc.path, err)
		}
		body, _ := io.ReadAll(resp.Body)
		resp.Body.Close()

		if resp.StatusCode != http.StatusOK {
			t.Fatalf("HEAD %s = %d, want 200", tc.path, resp.StatusCode)
		}
		if ct := resp.Header.Get("Content-Type"); ct != tc.contentType {
			t.Fatalf("HEAD %s Content-Type = %q, want %q", tc.path, ct, tc.contentType)
		}
		if len(body) != 0 {
			t.Fatalf("HEAD %s answered with a body: %q", tc.path, body)
		}
	}

	if cc := mustHead(t, ts.URL+"/p/"+s).Header.Get("Cache-Control"); cc != "public, max-age=60" {
		t.Fatalf("HEAD /p/{slug} Cache-Control = %q", cc)
	}
	if code := mustHead(t, ts.URL+"/p/no-such-slug-aaaa").StatusCode; code != http.StatusNotFound {
		t.Fatalf("HEAD an unknown slug = %d, want 404", code)
	}
}

func mustHead(t *testing.T, url string) *http.Response {
	t.Helper()
	resp, err := http.Head(url)
	if err != nil {
		t.Fatalf("HEAD %s: %v", url, err)
	}
	resp.Body.Close()
	return resp
}

func TestVerify(t *testing.T) {
	h := newHarness(t)
	d := h.newDevice()
	s := "quiet-heron-k7m2"
	if rec := h.do(http.MethodPost, "/v1/pages", publishBody(d, &s, "<h1>hi</h1>", rootHex)); rec.Code != http.StatusOK {
		t.Fatal(rec.Body.String())
	}

	rec := h.do(http.MethodGet, "/v1/verify/"+s, nil)
	if rec.Code != http.StatusOK {
		t.Fatalf("got %d: %s", rec.Code, rec.Body.String())
	}
	out := decode[struct {
		Slug       string    `json:"slug"`
		Root       string    `json:"root"`
		FirstTS    time.Time `json:"first_ts"`
		LastTS     time.Time `json:"last_ts"`
		ReceivedAt time.Time `json:"received_at"`
		DeviceID   string    `json:"device_id"`
	}](t, rec)
	if out.Slug != s || out.Root != rootHex || out.DeviceID != d.id {
		t.Fatalf("unexpected verify body: %+v", out)
	}
	if out.FirstTS.IsZero() || out.LastTS.IsZero() || out.ReceivedAt.IsZero() {
		t.Fatalf("missing timestamps: %+v", out)
	}
	// No html leaks through the verify endpoint.
	if strings.Contains(rec.Body.String(), "<h1>") {
		t.Fatal("verify response contains page html")
	}

	if rec := h.do(http.MethodGet, "/v1/verify/no-such-slug-aa", nil); rec.Code != http.StatusNotFound {
		t.Fatalf("unknown slug: got %d, want 404", rec.Code)
	}
}

func deleteBody(d device, s string) map[string]any {
	return map[string]any{
		"device_id": d.id,
		"signature": hex.EncodeToString(ed25519.Sign(d.priv, sign.DeleteMessage(s))),
	}
}

func TestDeletePage(t *testing.T) {
	h := newHarness(t)
	d := h.newDevice()
	s := "quiet-heron-k7m2"
	if rec := h.do(http.MethodPost, "/v1/pages", publishBody(d, &s, "<h1>hi</h1>", rootHex)); rec.Code != http.StatusOK {
		t.Fatal(rec.Body.String())
	}

	if rec := h.do(http.MethodDelete, "/v1/pages/"+s, deleteBody(d, s)); rec.Code != http.StatusOK {
		t.Fatalf("delete: got %d: %s", rec.Code, rec.Body.String())
	}

	if rec := h.do(http.MethodGet, "/p/"+s, nil); rec.Code != http.StatusNotFound {
		t.Fatalf("deleted page still served: %d", rec.Code)
	}
	if rec := h.do(http.MethodGet, "/v1/verify/"+s, nil); rec.Code != http.StatusNotFound {
		t.Fatalf("verify of deleted page: got %d, want 404", rec.Code)
	}

	// The root row survives, which is the whole point of a soft delete.
	roots, err := h.mem.Roots(t.Context(), s)
	if err != nil {
		t.Fatal(err)
	}
	if len(roots) != 1 || roots[0].Root != rootHex {
		t.Fatalf("root history lost: %+v", roots)
	}

	// Deleting twice is fine.
	if rec := h.do(http.MethodDelete, "/v1/pages/"+s, deleteBody(d, s)); rec.Code != http.StatusOK {
		t.Fatalf("second delete: got %d", rec.Code)
	}
}

func TestDeleteRequiresOwnerSignature(t *testing.T) {
	h := newHarness(t)
	owner := h.newDevice()
	intruder := h.newDevice()
	s := "quiet-heron-k7m2"
	if rec := h.do(http.MethodPost, "/v1/pages", publishBody(owner, &s, "<h1>hi</h1>", rootHex)); rec.Code != http.StatusOK {
		t.Fatal(rec.Body.String())
	}

	// Correctly signed, but by a device that does not own the slug.
	if rec := h.do(http.MethodDelete, "/v1/pages/"+s, deleteBody(intruder, s)); rec.Code != http.StatusForbidden {
		t.Fatalf("intruder delete: got %d, want 403: %s", rec.Code, rec.Body.String())
	}
	// Owner, but the signature is over a different slug.
	bad := deleteBody(owner, "some-other-slug")
	if rec := h.do(http.MethodDelete, "/v1/pages/"+s, bad); rec.Code != http.StatusUnauthorized {
		t.Fatalf("wrong-message signature: got %d, want 401", rec.Code)
	}
	if rec := h.do(http.MethodGet, "/p/"+s, nil); rec.Code != http.StatusOK {
		t.Fatalf("page was deleted by a failed request: %d", rec.Code)
	}

	if rec := h.do(http.MethodDelete, "/v1/pages/no-such-slug-aa", deleteBody(owner, "no-such-slug-aa")); rec.Code != http.StatusNotFound {
		t.Fatalf("unknown slug: got %d, want 404", rec.Code)
	}
}

func lsSign(secret string, body []byte) string {
	mac := hmac.New(sha256.New, []byte(secret))
	mac.Write(body)
	return hex.EncodeToString(mac.Sum(nil))
}

func TestLemonSqueezyWebhook(t *testing.T) {
	h := newHarness(t)
	raw := []byte(`{"meta":{"event_name":"order_created"},"data":{"id":"1"}}`)

	rec := h.do(http.MethodPost, "/v1/webhooks/lemonsqueezy", raw, [2]string{"X-Signature", lsSign(lsSecret, raw)})
	if rec.Code != http.StatusOK {
		t.Fatalf("got %d: %s", rec.Code, rec.Body.String())
	}
	events := h.mem.BillingEvents()
	if len(events) != 1 || !bytes.Equal(events[0], raw) {
		t.Fatalf("raw payload not stored verbatim: %q", events)
	}
}

func TestLemonSqueezyWebhookRejects(t *testing.T) {
	h := newHarness(t)
	raw := []byte(`{"meta":{"event_name":"order_created"}}`)

	t.Run("missing signature", func(t *testing.T) {
		if rec := h.do(http.MethodPost, "/v1/webhooks/lemonsqueezy", raw); rec.Code != http.StatusUnauthorized {
			t.Fatalf("got %d, want 401", rec.Code)
		}
	})
	t.Run("wrong signature", func(t *testing.T) {
		if rec := h.do(http.MethodPost, "/v1/webhooks/lemonsqueezy", raw, [2]string{"X-Signature", lsSign("other", raw)}); rec.Code != http.StatusUnauthorized {
			t.Fatalf("got %d, want 401", rec.Code)
		}
	})
	t.Run("signature over different body", func(t *testing.T) {
		sig := lsSign(lsSecret, []byte(`{"meta":{}}`))
		if rec := h.do(http.MethodPost, "/v1/webhooks/lemonsqueezy", raw, [2]string{"X-Signature", sig}); rec.Code != http.StatusUnauthorized {
			t.Fatalf("got %d, want 401", rec.Code)
		}
	})
	t.Run("valid signature over non-json", func(t *testing.T) {
		body := []byte("not json")
		if rec := h.do(http.MethodPost, "/v1/webhooks/lemonsqueezy", body, [2]string{"X-Signature", lsSign(lsSecret, body)}); rec.Code != http.StatusBadRequest {
			t.Fatalf("got %d, want 400", rec.Code)
		}
	})
	if len(h.mem.BillingEvents()) != 0 {
		t.Fatal("a rejected webhook was stored")
	}
}

func TestLemonSqueezyWebhookWithoutSecretIs503(t *testing.T) {
	mem := store.NewMemory()
	srv, err := httpapi.NewRouter(mem, httpapi.Config{PublicBaseURL: baseURL, Logger: quietLogger()})
	if err != nil {
		t.Fatal(err)
	}
	h := &harness{t: t, mem: mem, srv: srv}
	raw := []byte(`{"meta":{}}`)
	if rec := h.do(http.MethodPost, "/v1/webhooks/lemonsqueezy", raw, [2]string{"X-Signature", lsSign("anything", raw)}); rec.Code != http.StatusServiceUnavailable {
		t.Fatalf("got %d, want 503", rec.Code)
	}
}

func TestHealthz(t *testing.T) {
	h := newHarness(t)
	if rec := h.do(http.MethodGet, "/healthz", nil); rec.Code != http.StatusOK {
		t.Fatalf("got %d: %s", rec.Code, rec.Body.String())
	}

	h.mem.PingErr = io.ErrUnexpectedEOF
	rec := h.do(http.MethodGet, "/healthz", nil)
	if rec.Code != http.StatusServiceUnavailable {
		t.Fatalf("db down: got %d, want 503", rec.Code)
	}
}

func TestRequestIDHeader(t *testing.T) {
	h := newHarness(t)
	rec := h.do(http.MethodGet, "/healthz", nil)
	if rec.Header().Get("X-Request-Id") == "" {
		t.Fatal("no X-Request-Id on the response")
	}
}

func TestNewRouterRequiresPublicBaseURL(t *testing.T) {
	if _, err := httpapi.NewRouter(store.NewMemory(), httpapi.Config{}); err == nil {
		t.Fatal("expected an error when PUBLIC_BASE_URL is missing")
	}
	if _, err := httpapi.NewRouter(store.NewMemory(), httpapi.Config{PublicBaseURL: "not a url"}); err == nil {
		t.Fatal("expected an error for a malformed PUBLIC_BASE_URL")
	}
}

func TestPublicBaseURLTrailingSlashIsTrimmed(t *testing.T) {
	mem := store.NewMemory()
	srv, err := httpapi.NewRouter(mem, httpapi.Config{PublicBaseURL: baseURL + "/", Logger: quietLogger()})
	if err != nil {
		t.Fatal(err)
	}
	h := &harness{t: t, mem: mem, srv: srv}
	d := h.newDevice()
	rec := h.do(http.MethodPost, "/v1/pages", publishBody(d, nil, "<h1>x</h1>", rootHex))
	out := decode[map[string]any](t, rec)
	if url, _ := out["url"].(string); !strings.HasPrefix(url, baseURL+"/p/") || strings.Contains(url, "//p/") {
		t.Fatalf("url = %q", url)
	}
}
