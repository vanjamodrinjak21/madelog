package httpapi

import (
	"errors"
	"net/http"
	"strings"
	"time"

	"github.com/madelog/server/internal/store"
)

// stampMarker is where the page's print-only stamp paragraph ends. The CLI
// renders it as the last thing inside <main>, so the exported moment goes in
// right before it closes.
const stampMarker = "</p>\n\n</main>"

// stampFormat matches the `stamp` filter the page uses for its own dates.
const stampFormat = "2 Jan 2006, 15:04 UTC"

// handleServePDF answers GET /p/{slug}.pdf: the page as a PDF, for devices on
// the Pro plan, stamped with the moment it was exported. PDFs are cached per
// root and per day, so a re-publish re-renders and the stamp is never more
// than a day old.
func (a *api) handleServePDF(w http.ResponseWriter, r *http.Request, s string) {
	if a.renderer == nil {
		writeError(w, r, http.StatusServiceUnavailable, "renderer_unavailable", "PDF export is not configured on this server")
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
	dev, err := a.store.Device(r.Context(), page.DeviceID)
	if err != nil {
		a.internalError(w, r, "loading device", err)
		return
	}
	now := a.now().UTC()
	if !dev.Pro(now) {
		writeError(w, r, http.StatusPaymentRequired, "pro_required", "PDF export is part of madelog Pro")
		return
	}

	day := now.Format("2006-01-02")
	if cached, err := a.store.PagePDF(r.Context(), s, page.Root, day); err == nil {
		servePDF(w, s, cached.PDF)
		return
	} else if !errors.Is(err, store.ErrNotFound) {
		a.internalError(w, r, "loading cached pdf", err)
		return
	}

	pdf, err := a.renderer.Render(r.Context(), []byte(stampExported(page.HTML, now)))
	if err != nil {
		a.log.ErrorContext(r.Context(), "rendering pdf", "slug", s, "err", err)
		writeError(w, r, http.StatusServiceUnavailable, "renderer_unavailable", "the PDF could not be rendered right now; the page itself is unaffected")
		return
	}
	if err := a.store.PutPagePDF(r.Context(), s, page.Root, day, pdf); err != nil {
		// The reader still gets their PDF; only the cache missed.
		a.log.ErrorContext(r.Context(), "caching pdf", "slug", s, "err", err)
	}
	servePDF(w, s, pdf)
}

func servePDF(w http.ResponseWriter, s string, pdf []byte) {
	w.Header().Set("Content-Type", "application/pdf")
	w.Header().Set("Content-Disposition", `inline; filename="`+s+`.pdf"`)
	w.Header().Set("Cache-Control", "private, max-age=0")
	w.Header().Set("X-Content-Type-Options", "nosniff")
	w.WriteHeader(http.StatusOK)
	_, _ = w.Write(pdf)
}

// stampExported adds " · exported <now>" to the page's print stamp. A page
// rendered before the stamp existed gets a minimal one before </main>.
func stampExported(html string, now time.Time) string {
	exported := "exported " + now.UTC().Format(stampFormat)
	if i := strings.LastIndex(html, stampMarker); i >= 0 && strings.Contains(html, `class="stamp"`) {
		return html[:i] + " · " + exported + html[i:]
	}
	if i := strings.LastIndex(html, "</main>"); i >= 0 {
		return html[:i] + `<p class="stamp" aria-hidden="true">` + exported + "</p>\n" + html[i:]
	}
	return html + `<p class="stamp" aria-hidden="true">` + exported + "</p>\n"
}
