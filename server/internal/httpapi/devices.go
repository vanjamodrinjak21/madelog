package httpapi

import (
	"net/http"

	"github.com/pop-dev/popserver/internal/sign"
)

type createDeviceRequest struct {
	PublicKey string `json:"public_key"`
}

type createDeviceResponse struct {
	DeviceID string `json:"device_id"`
}

// handleCreateDevice registers an ed25519 public key. Idempotent: the same key
// always maps to the same device id, so `pop init` can call it on every run.
func (a *api) handleCreateDevice(w http.ResponseWriter, r *http.Request) {
	var req createDeviceRequest
	if tooLarge, err := decodeJSON(w, r, 4<<10, &req); err != nil {
		if tooLarge {
			writeError(w, r, http.StatusRequestEntityTooLarge, "too_large", "request body is too large")
			return
		}
		writeError(w, r, http.StatusBadRequest, "bad_request", "body must be a JSON object with a public_key field")
		return
	}

	pub, err := sign.ParsePublicKeyHex(req.PublicKey)
	if err != nil {
		writeError(w, r, http.StatusBadRequest, "bad_public_key", "public_key must be a hex-encoded 32-byte ed25519 public key")
		return
	}

	dev, err := a.store.UpsertDevice(r.Context(), pub)
	if err != nil {
		a.internalError(w, r, "upserting device", err)
		return
	}
	writeJSON(w, r, http.StatusOK, createDeviceResponse{DeviceID: dev.ID.String()})
}
