package httpapi

import (
	"encoding/json"
	"errors"
	"io"
	"log/slog"
	"net/http"

	"github.com/go-chi/chi/v5/middleware"
)

type errorBody struct {
	Error   string `json:"error"`
	Message string `json:"message"`
}

func writeJSON(w http.ResponseWriter, r *http.Request, status int, v any) {
	buf, err := json.Marshal(v)
	if err != nil {
		slog.ErrorContext(r.Context(), "encoding response", slog.Any("err", err))
		http.Error(w, `{"error":"internal","message":"could not encode response"}`, http.StatusInternalServerError)
		return
	}
	w.Header().Set("Content-Type", "application/json; charset=utf-8")
	w.WriteHeader(status)
	_, _ = w.Write(buf)
}

func writeError(w http.ResponseWriter, r *http.Request, status int, code, msg string) {
	writeJSON(w, r, status, errorBody{Error: code, Message: msg})
}

// internalError logs the cause and answers with a body that leaks nothing.
func (a *api) internalError(w http.ResponseWriter, r *http.Request, what string, err error) {
	a.log.ErrorContext(r.Context(), what,
		slog.String("request_id", middleware.GetReqID(r.Context())),
		slog.Any("err", err),
	)
	writeError(w, r, http.StatusInternalServerError, "internal", "something went wrong on our side")
}

// decodeJSON reads a JSON request body under a hard size limit. It reports
// tooLarge separately so the caller can answer 413 instead of 400.
func decodeJSON(w http.ResponseWriter, r *http.Request, limit int64, dst any) (tooLarge bool, err error) {
	r.Body = http.MaxBytesReader(w, r.Body, limit)
	dec := json.NewDecoder(r.Body)
	dec.DisallowUnknownFields()
	if err := dec.Decode(dst); err != nil {
		var mbe *http.MaxBytesError
		if errors.As(err, &mbe) {
			return true, err
		}
		return false, err
	}
	// A second value in the stream means the client sent something odd.
	if err := dec.Decode(&struct{}{}); !errors.Is(err, io.EOF) {
		var mbe *http.MaxBytesError
		if errors.As(err, &mbe) {
			return true, err
		}
		return false, errors.New("body must contain exactly one JSON object")
	}
	return false, nil
}
