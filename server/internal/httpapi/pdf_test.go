package httpapi_test

import (
	"context"
	"errors"
	"net/http"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/google/uuid"

	"github.com/madelog/server/internal/httpapi"
	"github.com/madelog/server/internal/store"
)

// fakeRenderer stands in for Gotenberg: it records the HTML it was given and
// answers with something that starts like a PDF.
type fakeRenderer struct {
	mu    sync.Mutex
	calls []string
	err   error
}

func (f *fakeRenderer) Render(_ context.Context, html []byte) ([]byte, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.calls = append(f.calls, string(html))
	if f.err != nil {
		return nil, f.err
	}
	return []byte("%PDF-1.4\n%fake\n"), nil
}

func (f *fakeRenderer) count() int {
	f.mu.Lock()
	defer f.mu.Unlock()
	return len(f.calls)
}

var pdfNow = time.Date(2026, 9, 21, 10, 41, 0, 0, time.UTC)

func newPDFHarness(t *testing.T, r *fakeRenderer) *harness {
	t.Helper()
	mem := store.NewMemory()
	cfg := httpapi.Config{
		PublicBaseURL: baseURL,
		Logger:        quietLogger(),
		Now:           func() time.Time { return pdfNow },
	}
	if r != nil {
		cfg.Renderer = r
	}
	srv, err := httpapi.NewRouter(mem, cfg)
	if err != nil {
		t.Fatal(err)
	}
	return &harness{t: t, mem: mem, srv: srv}
}

// The HTML the CLI publishes ends its <main> with the print-only stamp.
const stampedPage = `<!doctype html><html><body><main class="page"><h1>hi</h1>
  <p class="stamp" aria-hidden="true">ledger · active · root 3d9f1c0a4b5e</p>

</main>
<script>1</script></body></html>`

func (h *harness) makePro(id string, until *time.Time) {
	h.t.Helper()
	src := "manual"
	if _, err := h.mem.SetDevicePlan(h.t.Context(), uuid.MustParse(id), store.PlanPro, &src, until); err != nil {
		h.t.Fatal(err)
	}
}

func TestPDFRequiresPro(t *testing.T) {
	r := &fakeRenderer{}
	h := newPDFHarness(t, r)
	d := h.newDevice()
	s := "quiet-heron-k7m2"
	if rec := h.do(http.MethodPost, "/v1/pages", publishBody(d, &s, stampedPage, rootHex)); rec.Code != http.StatusOK {
		t.Fatal(rec.Body.String())
	}

	rec := h.do(http.MethodGet, "/p/"+s+".pdf", nil)
	if rec.Code != http.StatusPaymentRequired {
		t.Fatalf("free device: got %d, want 402: %s", rec.Code, rec.Body.String())
	}
	if got := decode[map[string]string](t, rec)["error"]; got != "pro_required" {
		t.Fatalf("error = %q", got)
	}
	if r.count() != 0 {
		t.Fatal("a free page was rendered")
	}

	// An expired plan is a free plan.
	past := pdfNow.Add(-time.Hour)
	h.makePro(d.id, &past)
	if rec := h.do(http.MethodGet, "/p/"+s+".pdf", nil); rec.Code != http.StatusPaymentRequired {
		t.Fatalf("expired plan: got %d, want 402", rec.Code)
	}

	// The HTML page itself is never gated.
	if rec := h.do(http.MethodGet, "/p/"+s, nil); rec.Code != http.StatusOK {
		t.Fatalf("html page: got %d", rec.Code)
	}
}

func TestPDFRendersStampsAndCaches(t *testing.T) {
	r := &fakeRenderer{}
	h := newPDFHarness(t, r)
	d := h.newDevice()
	h.makePro(d.id, nil)
	s := "quiet-heron-k7m2"
	if rec := h.do(http.MethodPost, "/v1/pages", publishBody(d, &s, stampedPage, rootHex)); rec.Code != http.StatusOK {
		t.Fatal(rec.Body.String())
	}

	rec := h.do(http.MethodGet, "/p/"+s+".pdf", nil)
	if rec.Code != http.StatusOK {
		t.Fatalf("got %d: %s", rec.Code, rec.Body.String())
	}
	if ct := rec.Header().Get("Content-Type"); ct != "application/pdf" {
		t.Fatalf("Content-Type = %q", ct)
	}
	if cd := rec.Header().Get("Content-Disposition"); cd != `inline; filename="quiet-heron-k7m2.pdf"` {
		t.Fatalf("Content-Disposition = %q", cd)
	}
	if cc := rec.Header().Get("Cache-Control"); cc != "private, max-age=0" {
		t.Fatalf("Cache-Control = %q", cc)
	}
	if !strings.HasPrefix(rec.Body.String(), "%PDF") {
		t.Fatalf("body is not a pdf: %q", rec.Body.String())
	}

	// The renderer saw the page with the export moment on its stamp, inside
	// the stamp paragraph, and nothing else changed.
	if r.count() != 1 {
		t.Fatalf("renderer called %d times, want 1", r.count())
	}
	want := `<p class="stamp" aria-hidden="true">ledger · active · root 3d9f1c0a4b5e · exported 21 Sep 2026, 10:41 UTC</p>

</main>`
	if !strings.Contains(r.calls[0], want) {
		t.Fatalf("stamp not injected:\n%s", r.calls[0])
	}
	if strings.Count(r.calls[0], "exported") != 1 || !strings.Contains(r.calls[0], "<script>1</script>") {
		t.Fatalf("html changed beyond the stamp:\n%s", r.calls[0])
	}

	// Second request on the same day and root: served from the cache.
	if rec := h.do(http.MethodGet, "/p/"+s+".pdf", nil); rec.Code != http.StatusOK {
		t.Fatalf("cached: got %d", rec.Code)
	}
	if r.count() != 1 {
		t.Fatalf("cache miss: renderer called %d times", r.count())
	}
	if n := h.mem.PagePDFCount(s); n != 1 {
		t.Fatalf("%d pdfs cached, want 1", n)
	}

	// A re-publish changes the root, so the next PDF is rendered afresh and
	// the old one is swept.
	if rec := h.do(http.MethodPost, "/v1/pages", publishBody(d, &s, stampedPage, rootHexB)); rec.Code != http.StatusOK {
		t.Fatal(rec.Body.String())
	}
	if rec := h.do(http.MethodGet, "/p/"+s+".pdf", nil); rec.Code != http.StatusOK {
		t.Fatalf("after republish: got %d", rec.Code)
	}
	if r.count() != 2 {
		t.Fatalf("republish did not re-render: %d calls", r.count())
	}
	if n := h.mem.PagePDFCount(s); n != 1 {
		t.Fatalf("stale pdf kept: %d cached", n)
	}
}

func TestPDFStampsPagesWithoutAStampParagraph(t *testing.T) {
	r := &fakeRenderer{}
	h := newPDFHarness(t, r)
	d := h.newDevice()
	h.makePro(d.id, nil)
	s := "quiet-heron-k7m2"
	old := `<!doctype html><html><body><main class="page"><h1>old page</h1></main></body></html>`
	if rec := h.do(http.MethodPost, "/v1/pages", publishBody(d, &s, old, rootHex)); rec.Code != http.StatusOK {
		t.Fatal(rec.Body.String())
	}
	if rec := h.do(http.MethodGet, "/p/"+s+".pdf", nil); rec.Code != http.StatusOK {
		t.Fatalf("got %d: %s", rec.Code, rec.Body.String())
	}
	want := `<h1>old page</h1><p class="stamp" aria-hidden="true">exported 21 Sep 2026, 10:41 UTC</p>
</main>`
	if !strings.Contains(r.calls[0], want) {
		t.Fatalf("minimal stamp not added:\n%s", r.calls[0])
	}
}

func TestPDFErrors(t *testing.T) {
	t.Run("no renderer configured", func(t *testing.T) {
		h := newPDFHarness(t, nil)
		d := h.newDevice()
		h.makePro(d.id, nil)
		s := "quiet-heron-k7m2"
		if rec := h.do(http.MethodPost, "/v1/pages", publishBody(d, &s, stampedPage, rootHex)); rec.Code != http.StatusOK {
			t.Fatal(rec.Body.String())
		}
		rec := h.do(http.MethodGet, "/p/"+s+".pdf", nil)
		if rec.Code != http.StatusServiceUnavailable {
			t.Fatalf("got %d, want 503: %s", rec.Code, rec.Body.String())
		}
	})

	t.Run("renderer fails", func(t *testing.T) {
		r := &fakeRenderer{err: errors.New("chromium went away")}
		h := newPDFHarness(t, r)
		d := h.newDevice()
		h.makePro(d.id, nil)
		s := "quiet-heron-k7m2"
		if rec := h.do(http.MethodPost, "/v1/pages", publishBody(d, &s, stampedPage, rootHex)); rec.Code != http.StatusOK {
			t.Fatal(rec.Body.String())
		}
		rec := h.do(http.MethodGet, "/p/"+s+".pdf", nil)
		if rec.Code != http.StatusServiceUnavailable {
			t.Fatalf("got %d, want 503: %s", rec.Code, rec.Body.String())
		}
		if got := decode[map[string]string](t, rec)["error"]; got != "renderer_unavailable" {
			t.Fatalf("error = %q", got)
		}
		if n := h.mem.PagePDFCount(s); n != 0 {
			t.Fatalf("a failed render was cached: %d", n)
		}
	})

	t.Run("unknown, deleted and malformed slugs", func(t *testing.T) {
		r := &fakeRenderer{}
		h := newPDFHarness(t, r)
		d := h.newDevice()
		h.makePro(d.id, nil)
		s := "quiet-heron-k7m2"
		if rec := h.do(http.MethodGet, "/p/no-such-slug-aaaa.pdf", nil); rec.Code != http.StatusNotFound {
			t.Fatalf("unknown: got %d, want 404", rec.Code)
		}
		if rec := h.do(http.MethodGet, "/p/..%2F..%2Fetc.pdf", nil); rec.Code != http.StatusNotFound {
			t.Fatalf("malformed: got %d, want 404", rec.Code)
		}
		if rec := h.do(http.MethodPost, "/v1/pages", publishBody(d, &s, stampedPage, rootHex)); rec.Code != http.StatusOK {
			t.Fatal(rec.Body.String())
		}
		if rec := h.do(http.MethodDelete, "/v1/pages/"+s, deleteBody(d, s)); rec.Code != http.StatusOK {
			t.Fatal(rec.Body.String())
		}
		if rec := h.do(http.MethodGet, "/p/"+s+".pdf", nil); rec.Code != http.StatusNotFound {
			t.Fatalf("deleted: got %d, want 404", rec.Code)
		}
		if r.count() != 0 {
			t.Fatal("something was rendered")
		}
	})
}
