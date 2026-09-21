package httpapi

import (
	"errors"
	"net/http"
	"strings"
	"time"

	"github.com/go-chi/chi/v5"
	"github.com/google/uuid"

	"github.com/madelog/server/internal/licensing"
	"github.com/madelog/server/internal/sign"
	"github.com/madelog/server/internal/store"
)

type deviceResponse struct {
	DeviceID   string     `json:"device_id"`
	Plan       string     `json:"plan"`
	PlanSource *string    `json:"plan_source,omitempty"`
	PlanUntil  *time.Time `json:"plan_until,omitempty"`
}

func deviceOut(d store.Device) deviceResponse {
	out := deviceResponse{DeviceID: d.ID.String(), Plan: d.Plan, PlanSource: d.PlanSource}
	if d.PlanUntil != nil {
		t := d.PlanUntil.UTC()
		out.PlanUntil = &t
	}
	return out
}

// handleGetDevice answers with a device's plan. A device id is a public
// identifier and a plan is not a secret, so this needs no signature.
func (a *api) handleGetDevice(w http.ResponseWriter, r *http.Request) {
	id, err := uuid.Parse(chi.URLParam(r, "id"))
	if err != nil {
		writeError(w, r, http.StatusBadRequest, "bad_device_id", "device id must be a UUID")
		return
	}
	dev, err := a.store.Device(r.Context(), id)
	if errors.Is(err, store.ErrNotFound) {
		writeError(w, r, http.StatusNotFound, "unknown_device", "no such device")
		return
	} else if err != nil {
		a.internalError(w, r, "loading device", err)
		return
	}
	writeJSON(w, r, http.StatusOK, deviceOut(dev))
}

type activatePlanRequest struct {
	LicenseKey string `json:"license_key"`
	Signature  string `json:"signature"`
}

// handleActivatePlan puts a device on the Pro plan by activating a
// LemonSqueezy license key for it. The request is signed by the device, so a
// key can only ever be bound to a machine that holds the matching private key.
func (a *api) handleActivatePlan(w http.ResponseWriter, r *http.Request) {
	id, err := uuid.Parse(chi.URLParam(r, "id"))
	if err != nil {
		writeError(w, r, http.StatusBadRequest, "bad_device_id", "device id must be a UUID")
		return
	}
	var req activatePlanRequest
	if tooLarge, err := decodeJSON(w, r, 4<<10, &req); err != nil {
		if tooLarge {
			writeError(w, r, http.StatusRequestEntityTooLarge, "too_large", "request body is too large")
			return
		}
		writeError(w, r, http.StatusBadRequest, "bad_request", "body must be a JSON object with license_key and signature")
		return
	}
	key := strings.TrimSpace(req.LicenseKey)
	if key == "" || len(key) > 200 || key != req.LicenseKey {
		writeError(w, r, http.StatusBadRequest, "bad_license_key", "license_key is required, without surrounding whitespace")
		return
	}
	if _, err := sign.ParseSignatureHex(req.Signature); err != nil {
		writeError(w, r, http.StatusBadRequest, "bad_signature", "signature must be a hex-encoded 64-byte ed25519 signature")
		return
	}

	dev, err := a.store.Device(r.Context(), id)
	if errors.Is(err, store.ErrNotFound) {
		writeError(w, r, http.StatusNotFound, "unknown_device", "register the device with POST /v1/devices first")
		return
	} else if err != nil {
		a.internalError(w, r, "loading device", err)
		return
	}
	if err := sign.VerifyHex(dev.PublicKey, sign.PlanMessage(id.String(), key), req.Signature); err != nil {
		writeError(w, r, http.StatusUnauthorized, "bad_signature", "signature does not verify for this device")
		return
	}
	if a.activator == nil {
		writeError(w, r, http.StatusServiceUnavailable, "not_configured", "license activation is not configured on this server")
		return
	}

	lic, err := a.activator.Activate(r.Context(), key, id.String())
	if errors.Is(err, licensing.ErrRejected) {
		msg := strings.TrimPrefix(err.Error(), licensing.ErrRejected.Error()+": ")
		writeError(w, r, http.StatusForbidden, "license_rejected", msg)
		return
	}
	if err != nil {
		a.log.ErrorContext(r.Context(), "activating license", "device", id, "err", err)
		writeError(w, r, http.StatusBadGateway, "licensing_unavailable", "the license could not be checked right now; nothing changed")
		return
	}
	if a.cfg.LicenseStoreID != 0 && lic.StoreID != a.cfg.LicenseStoreID {
		writeError(w, r, http.StatusForbidden, "license_rejected", "this license key is not for madelog")
		return
	}
	if lic.Status != "active" {
		writeError(w, r, http.StatusForbidden, "license_rejected", "this license key is "+lic.Status)
		return
	}

	source := "lemonsqueezy:" + lic.ID
	if lic.InstanceID != "" {
		source += ":" + lic.InstanceID
	}
	updated, err := a.store.SetDevicePlan(r.Context(), id, store.PlanPro, &source, lic.ExpiresAt)
	if err != nil {
		a.internalError(w, r, "setting plan", err)
		return
	}
	a.log.InfoContext(r.Context(), "plan activated", "device", id, "license", lic.ID, "until", lic.ExpiresAt)
	writeJSON(w, r, http.StatusOK, deviceOut(updated))
}
