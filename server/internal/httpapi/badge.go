package httpapi

import (
	"errors"
	"fmt"
	"net/http"

	"github.com/go-chi/chi/v5"

	"github.com/madelog/server/internal/sign"
	"github.com/madelog/server/internal/slug"
	"github.com/madelog/server/internal/store"
)

// handleBadge serves GET /p/{slug}/badge.svg: a static two-part pill for a
// README. Only the validated root from the store is interpolated, so no
// user-supplied html or text is reflected.
func (a *api) handleBadge(w http.ResponseWriter, r *http.Request) {
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
	if !sign.ValidRootHex(page.Root) {
		a.internalError(w, r, "badge root", errors.New("stored root is not 64 lowercase hex"))
		return
	}

	w.Header().Set("Content-Type", "image/svg+xml; charset=utf-8")
	w.Header().Set("Cache-Control", "public, max-age=300")
	w.Header().Set("X-Content-Type-Options", "nosniff")
	w.WriteHeader(http.StatusOK)
	_, _ = w.Write([]byte(badgeSVG(page.Root)))
}

// badgeSVG renders the badge. root must already be validated hex.
func badgeSVG(root string) string {
	const (
		left   = "madelog"
		height = 20
		char   = 7
		pad    = 10
	)
	right := "verified · " + root[:8]
	lw := len(left)*char + 2*pad
	rw := len([]rune(right))*char + 2*pad
	w := lw + rw
	return fmt.Sprintf(`<svg xmlns="http://www.w3.org/2000/svg" width="%[1]d" height="%[2]d" viewBox="0 0 %[1]d %[2]d" role="img" aria-label="madelog: %[7]s">`+
		`<title>madelog verified record, root %[8]s</title>`+
		`<rect width="%[3]d" height="%[2]d" fill="#111"/>`+
		`<rect x="%[9]s" y="0.5" width="%[10]s" height="%[11]s" fill="#fff" stroke="#111" stroke-width="1"/>`+
		`<g font-family="Verdana,DejaVu Sans,sans-serif" font-size="11" text-anchor="middle">`+
		`<text x="%[4]d" y="14" fill="#fff">%[5]s</text>`+
		`<text x="%[6]d" y="14" fill="#111">%[7]s</text>`+
		`</g></svg>`,
		w, height, lw, lw/2, left, lw+rw/2, right, root,
		fmt.Sprint(lw), fmt.Sprint(rw-1), fmt.Sprint(height-1))
}
