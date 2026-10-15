package httpapi_test

import (
	"encoding/xml"
	"net/http"
	"strings"
	"testing"
)

func TestBadgeServed(t *testing.T) {
	h := newHarness(t)
	d := h.newDevice()
	s := "quiet-heron-k7m2"
	pub := decode[map[string]any](t, h.do(http.MethodPost, "/v1/pages", publishBody(d, &s, "<p>x</p>", rootHex)))
	if pub["badge_url"] != baseURL+"/p/"+s+"/badge.svg" {
		t.Fatalf("badge_url = %v", pub["badge_url"])
	}

	for _, method := range []string{http.MethodGet, http.MethodHead} {
		rec := h.do(method, "/p/"+s+"/badge.svg", nil)
		if rec.Code != http.StatusOK {
			t.Fatalf("%s = %d", method, rec.Code)
		}
		for k, want := range map[string]string{
			"Content-Type":           "image/svg+xml; charset=utf-8",
			"Cache-Control":          "public, max-age=300",
			"X-Content-Type-Options": "nosniff",
		} {
			if got := rec.Header().Get(k); got != want {
				t.Errorf("%s %s = %q, want %q", method, k, got, want)
			}
		}
		if method == http.MethodHead {
			continue
		}
		body := rec.Body.String()
		if !strings.Contains(body, "verified · "+rootHex[:8]) || !strings.Contains(body, "root "+rootHex+"</title>") {
			t.Fatalf("badge missing root: %s", body)
		}
		dec := xml.NewDecoder(strings.NewReader(body))
		for {
			if _, err := dec.Token(); err != nil {
				if err.Error() != "EOF" {
					t.Fatalf("not well-formed xml: %v", err)
				}
				break
			}
		}
	}
}

func TestBadgeNotFound(t *testing.T) {
	h := newHarness(t)
	if rec := h.do(http.MethodGet, "/p/no-such-page/badge.svg", nil); rec.Code != http.StatusNotFound || strings.Contains(rec.Body.String(), "<svg") {
		t.Fatalf("unknown = %d %s", rec.Code, rec.Body.String())
	}

	d := h.newDevice()
	s := "quiet-heron-k7m2"
	h.do(http.MethodPost, "/v1/pages", publishBody(d, &s, "<p>x</p>", rootHex))
	if rec := h.do(http.MethodDelete, "/v1/pages/"+s, deleteBody(d, s)); rec.Code != http.StatusOK {
		t.Fatalf("delete = %d %s", rec.Code, rec.Body.String())
	}
	if rec := h.do(http.MethodGet, "/p/"+s+"/badge.svg", nil); rec.Code != http.StatusNotFound || strings.Contains(rec.Body.String(), "<svg") {
		t.Fatalf("deleted = %d %s", rec.Code, rec.Body.String())
	}
}
