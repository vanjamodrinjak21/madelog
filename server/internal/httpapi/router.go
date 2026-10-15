// Package httpapi wires the chi router, the middleware stack and every handler
// in spec section 5. It depends on store.Store, never on pgx, so the whole API
// can be exercised with an in-memory fake.
package httpapi

import (
	"errors"
	"fmt"
	"log/slog"
	"net/http"
	"net/url"
	"strings"
	"time"

	"github.com/go-chi/chi/v5"
	"github.com/go-chi/chi/v5/middleware"

	"github.com/madelog/server/internal/licensing"
	"github.com/madelog/server/internal/pdf"
	"github.com/madelog/server/internal/store"
)

// DefaultMaxHTMLBytes is the largest page body we accept, per spec section 5.
const DefaultMaxHTMLBytes = 2 << 20 // 2 MiB

// Config is everything the API needs from the environment.
type Config struct {
	// PublicBaseURL is the origin the returned page URLs are built from,
	// for example https://madelog.example. Required.
	PublicBaseURL string
	// LemonSqueezySecret signs the billing webhook. Empty means the webhook
	// is not configured and answers 503.
	LemonSqueezySecret string
	// MaxHTMLBytes overrides DefaultMaxHTMLBytes. Optional.
	MaxHTMLBytes int64
	// Logger defaults to slog.Default().
	Logger *slog.Logger
	// RequestTimeout defaults to 30s.
	RequestTimeout time.Duration
	// Renderer produces PDFs for GET /p/{slug}.pdf. Nil means the endpoint
	// answers 503.
	Renderer pdf.Renderer
	// Now defaults to time.Now. Tests set it to pin the export stamp.
	Now func() time.Time
	// Activator turns a license key into a plan. Nil means
	// POST /v1/devices/{id}/plan answers 503.
	Activator licensing.Activator
	// LicenseStoreID, when non-zero, is the only LemonSqueezy store whose
	// keys are accepted.
	LicenseStoreID int64
}

type api struct {
	store     store.Store
	cfg       Config
	log       *slog.Logger
	baseURL   string
	renderer  pdf.Renderer
	now       func() time.Time
	activator licensing.Activator
}

// badgeURL is the public address of a page's README badge.
func (a *api) badgeURL(slug string) string { return a.baseURL + "/p/" + slug + "/badge.svg" }

// pdfURL is the public address of a page's PDF.
func (a *api) pdfURL(slug string) string { return a.baseURL + "/p/" + slug + ".pdf" }

// NewRouter validates cfg and returns the fully wired handler.
func NewRouter(s store.Store, cfg Config) (http.Handler, error) {
	if s == nil {
		return nil, errors.New("httpapi: store is required")
	}
	base := strings.TrimRight(strings.TrimSpace(cfg.PublicBaseURL), "/")
	if base == "" {
		return nil, errors.New("httpapi: PUBLIC_BASE_URL is required")
	}
	u, err := url.Parse(base)
	if err != nil || u.Scheme == "" || u.Host == "" {
		return nil, fmt.Errorf("httpapi: PUBLIC_BASE_URL %q is not an absolute URL", cfg.PublicBaseURL)
	}
	if cfg.MaxHTMLBytes <= 0 {
		cfg.MaxHTMLBytes = DefaultMaxHTMLBytes
	}
	if cfg.RequestTimeout <= 0 {
		cfg.RequestTimeout = 30 * time.Second
	}
	if cfg.Logger == nil {
		cfg.Logger = slog.Default()
	}

	if cfg.Now == nil {
		cfg.Now = time.Now
	}

	a := &api{store: s, cfg: cfg, log: cfg.Logger, baseURL: base, renderer: cfg.Renderer, now: cfg.Now, activator: cfg.Activator}

	r := chi.NewRouter()
	r.Use(middleware.RequestID)
	r.Use(middleware.RealIP)
	r.Use(exposeRequestID)
	r.Use(requestLogger(a.log))
	r.Use(middleware.Recoverer)
	r.Use(middleware.Timeout(cfg.RequestTimeout))

	r.Get("/healthz", a.handleHealthz)
	// `curl -I` and every uptime checker send HEAD. net/http drops the body
	// for us, so the GET handlers answer both with the same headers.
	r.Head("/healthz", a.handleHealthz)
	r.Get("/p/{slug}", a.handleServePage)
	r.Head("/p/{slug}", a.handleServePage)
	r.Get("/p/{slug}/badge.svg", a.handleBadge)
	r.Head("/p/{slug}/badge.svg", a.handleBadge)

	r.Route("/v1", func(r chi.Router) {
		r.Post("/devices", a.handleCreateDevice)
		r.Get("/devices/{id}", a.handleGetDevice)
		r.Post("/devices/{id}/plan", a.handleActivatePlan)
		r.Post("/pages", a.handlePublishPage)
		r.Delete("/pages/{slug}", a.handleDeletePage)
		r.Get("/verify/{slug}", a.handleVerify)
		r.Post("/webhooks/lemonsqueezy", a.handleLemonSqueezyWebhook)
	})

	r.NotFound(func(w http.ResponseWriter, r *http.Request) {
		writeError(w, r, http.StatusNotFound, "not_found", "no such endpoint")
	})
	r.MethodNotAllowed(func(w http.ResponseWriter, r *http.Request) {
		writeError(w, r, http.StatusMethodNotAllowed, "method_not_allowed", "method not allowed on this endpoint")
	})

	return r, nil
}

// pageURL is the public address of a published page.
func (a *api) pageURL(slug string) string { return a.baseURL + "/p/" + slug }

// verifyURL is the public address of a page's verification record.
func (a *api) verifyURL(slug string) string { return a.baseURL + "/v1/verify/" + slug }
