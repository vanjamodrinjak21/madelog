package httpapi

import (
	"context"
	"net/http"
	"time"
)

type healthResponse struct {
	Status string `json:"status"`
}

// handleHealthz answers 200 only when the database answers too, so a rolling
// deploy does not send traffic to an instance that cannot serve anything.
func (a *api) handleHealthz(w http.ResponseWriter, r *http.Request) {
	ctx, cancel := context.WithTimeout(r.Context(), 2*time.Second)
	defer cancel()

	if err := a.store.Ping(ctx); err != nil {
		a.log.WarnContext(ctx, "healthz: database ping failed", "err", err)
		writeJSON(w, r, http.StatusServiceUnavailable, healthResponse{Status: "degraded"})
		return
	}
	writeJSON(w, r, http.StatusOK, healthResponse{Status: "ok"})
}
