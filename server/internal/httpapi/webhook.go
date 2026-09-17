package httpapi

import (
	"crypto/hmac"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"io"
	"net/http"
)

// maxWebhookBytes caps the billing payload we are willing to read.
const maxWebhookBytes = 1 << 20 // 1 MiB

// handleLemonSqueezyWebhook verifies the X-Signature header (hex HMAC-SHA256
// over the raw body, keyed with LEMONSQUEEZY_SECRET) and files the raw payload
// in billing_events. Nothing else happens in v1: no plan gating exists yet, so
// the endpoint is a verified stub that keeps the audit trail.
func (a *api) handleLemonSqueezyWebhook(w http.ResponseWriter, r *http.Request) {
	if a.cfg.LemonSqueezySecret == "" {
		writeError(w, r, http.StatusServiceUnavailable, "not_configured", "billing webhook is not configured")
		return
	}

	r.Body = http.MaxBytesReader(w, r.Body, maxWebhookBytes)
	raw, err := io.ReadAll(r.Body)
	if err != nil {
		var mbe *http.MaxBytesError
		if errors.As(err, &mbe) {
			writeError(w, r, http.StatusRequestEntityTooLarge, "too_large", "webhook body is too large")
			return
		}
		writeError(w, r, http.StatusBadRequest, "bad_request", "could not read body")
		return
	}

	if !validLemonSqueezySignature(a.cfg.LemonSqueezySecret, r.Header.Get("X-Signature"), raw) {
		writeError(w, r, http.StatusUnauthorized, "bad_signature", "X-Signature does not match the body")
		return
	}
	if !json.Valid(raw) {
		writeError(w, r, http.StatusBadRequest, "bad_request", "body must be JSON")
		return
	}

	if err := a.store.InsertBillingEvent(r.Context(), raw); err != nil {
		a.internalError(w, r, "storing billing event", err)
		return
	}
	writeJSON(w, r, http.StatusOK, map[string]bool{"ok": true})
}

// validLemonSqueezySignature compares in constant time.
func validLemonSqueezySignature(secret, header string, body []byte) bool {
	got, err := hex.DecodeString(header)
	if err != nil || len(got) != sha256.Size {
		return false
	}
	mac := hmac.New(sha256.New, []byte(secret))
	mac.Write(body)
	return hmac.Equal(got, mac.Sum(nil))
}
