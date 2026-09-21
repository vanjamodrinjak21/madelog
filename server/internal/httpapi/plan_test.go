package httpapi_test

import (
	"context"
	"crypto/ed25519"
	"encoding/hex"
	"errors"
	"net/http"
	"testing"
	"time"

	"github.com/google/uuid"

	"github.com/madelog/server/internal/httpapi"
	"github.com/madelog/server/internal/licensing"
	"github.com/madelog/server/internal/sign"
	"github.com/madelog/server/internal/store"
)

// fakeActivator stands in for LemonSqueezy.
type fakeActivator struct {
	lic   licensing.License
	err   error
	calls []string // "key|instance"
}

func (f *fakeActivator) Activate(_ context.Context, key, instance string) (licensing.License, error) {
	f.calls = append(f.calls, key+"|"+instance)
	if f.err != nil {
		return licensing.License{}, f.err
	}
	return f.lic, nil
}

func newPlanHarness(t *testing.T, act licensing.Activator, storeID int64) *harness {
	t.Helper()
	mem := store.NewMemory()
	cfg := httpapi.Config{
		PublicBaseURL:  baseURL,
		Logger:         quietLogger(),
		Now:            func() time.Time { return pdfNow },
		LicenseStoreID: storeID,
	}
	if act != nil {
		cfg.Activator = act
	}
	srv, err := httpapi.NewRouter(mem, cfg)
	if err != nil {
		t.Fatal(err)
	}
	return &harness{t: t, mem: mem, srv: srv}
}

func planBody(d device, key string) map[string]any {
	return map[string]any{
		"license_key": key,
		"signature":   hex.EncodeToString(ed25519.Sign(d.priv, sign.PlanMessage(d.id, key))),
	}
}

func TestActivatePlanMakesTheDevicePro(t *testing.T) {
	until := time.Date(2027, 9, 1, 0, 0, 0, 0, time.UTC)
	act := &fakeActivator{lic: licensing.License{ID: "41", Status: "active", ExpiresAt: &until, InstanceID: "inst-1", StoreID: 7, ProductID: 10}}
	h := newPlanHarness(t, act, 7)
	d := h.newDevice()

	// Before: free, and the plan is readable.
	got := decode[map[string]any](t, h.do(http.MethodGet, "/v1/devices/"+d.id, nil))
	if got["plan"] != "free" {
		t.Fatalf("new device plan = %v", got["plan"])
	}

	rec := h.do(http.MethodPost, "/v1/devices/"+d.id+"/plan", planBody(d, "KEY-ABC"))
	if rec.Code != http.StatusOK {
		t.Fatalf("got %d: %s", rec.Code, rec.Body.String())
	}
	out := decode[struct {
		DeviceID   string    `json:"device_id"`
		Plan       string    `json:"plan"`
		PlanSource string    `json:"plan_source"`
		PlanUntil  time.Time `json:"plan_until"`
	}](t, rec)
	if out.DeviceID != d.id || out.Plan != "pro" || out.PlanSource != "lemonsqueezy:41:inst-1" || !out.PlanUntil.Equal(until) {
		t.Fatalf("unexpected: %+v", out)
	}
	if len(act.calls) != 1 || act.calls[0] != "KEY-ABC|"+d.id {
		t.Fatalf("activator calls = %v", act.calls)
	}

	// The PDF is now allowed, and publish says so.
	s := "quiet-heron-k7m2"
	pub := decode[map[string]any](t, h.do(http.MethodPost, "/v1/pages", publishBody(d, &s, stampedPage, rootHex)))
	if pub["plan"] != "pro" || pub["pdf_url"] != baseURL+"/p/"+s+".pdf" {
		t.Fatalf("publish response = %v", pub)
	}
	if rec := h.do(http.MethodGet, "/p/"+s+".pdf", nil); rec.Code != http.StatusServiceUnavailable {
		t.Fatalf("pro device without a renderer: got %d, want 503 (not 402)", rec.Code)
	}
}

func TestPublishResponseCarriesThePlan(t *testing.T) {
	h := newPlanHarness(t, nil, 0)
	d := h.newDevice()
	pub := decode[map[string]any](t, h.do(http.MethodPost, "/v1/pages", publishBody(d, nil, "<h1>x</h1>", rootHex)))
	if pub["plan"] != "free" {
		t.Fatalf("plan = %v", pub["plan"])
	}
	if _, has := pub["pdf_url"]; has {
		t.Fatalf("a free publish must not advertise a pdf: %v", pub)
	}
}

func TestActivatePlanRejections(t *testing.T) {
	ok := licensing.License{ID: "41", Status: "active", StoreID: 7}

	t.Run("signature by another device", func(t *testing.T) {
		h := newPlanHarness(t, &fakeActivator{lic: ok}, 7)
		d, other := h.newDevice(), h.newDevice()
		body := planBody(other, "KEY")
		if rec := h.do(http.MethodPost, "/v1/devices/"+d.id+"/plan", body); rec.Code != http.StatusUnauthorized {
			t.Fatalf("got %d, want 401: %s", rec.Code, rec.Body.String())
		}
	})
	t.Run("signature over another key", func(t *testing.T) {
		h := newPlanHarness(t, &fakeActivator{lic: ok}, 7)
		d := h.newDevice()
		body := planBody(d, "KEY")
		body["license_key"] = "OTHER"
		if rec := h.do(http.MethodPost, "/v1/devices/"+d.id+"/plan", body); rec.Code != http.StatusUnauthorized {
			t.Fatalf("got %d, want 401", rec.Code)
		}
	})
	t.Run("lemonsqueezy says no", func(t *testing.T) {
		act := &fakeActivator{err: errors.New("licensing: rejected: This license key has reached the activation limit.")}
		act.err = errors.Join(licensing.ErrRejected, act.err)
		h := newPlanHarness(t, act, 7)
		d := h.newDevice()
		rec := h.do(http.MethodPost, "/v1/devices/"+d.id+"/plan", planBody(d, "KEY"))
		if rec.Code != http.StatusForbidden {
			t.Fatalf("got %d, want 403: %s", rec.Code, rec.Body.String())
		}
		if got := decode[map[string]string](t, rec)["error"]; got != "license_rejected" {
			t.Fatalf("error = %q", got)
		}
		if dev, _ := h.mem.Device(t.Context(), mustUUID(t, d.id)); dev.Plan != "free" {
			t.Fatal("a rejected key changed the plan")
		}
	})
	t.Run("wrong store", func(t *testing.T) {
		h := newPlanHarness(t, &fakeActivator{lic: licensing.License{ID: "1", Status: "active", StoreID: 99}}, 7)
		d := h.newDevice()
		if rec := h.do(http.MethodPost, "/v1/devices/"+d.id+"/plan", planBody(d, "KEY")); rec.Code != http.StatusForbidden {
			t.Fatalf("got %d, want 403", rec.Code)
		}
	})
	t.Run("key not active", func(t *testing.T) {
		h := newPlanHarness(t, &fakeActivator{lic: licensing.License{ID: "1", Status: "disabled", StoreID: 7}}, 7)
		d := h.newDevice()
		if rec := h.do(http.MethodPost, "/v1/devices/"+d.id+"/plan", planBody(d, "KEY")); rec.Code != http.StatusForbidden {
			t.Fatalf("got %d, want 403", rec.Code)
		}
	})
	t.Run("lemonsqueezy down", func(t *testing.T) {
		h := newPlanHarness(t, &fakeActivator{err: errors.New("licensing: lemonsqueezy unreachable")}, 7)
		d := h.newDevice()
		rec := h.do(http.MethodPost, "/v1/devices/"+d.id+"/plan", planBody(d, "KEY"))
		if rec.Code != http.StatusBadGateway {
			t.Fatalf("got %d, want 502: %s", rec.Code, rec.Body.String())
		}
	})
	t.Run("not configured", func(t *testing.T) {
		h := newPlanHarness(t, nil, 0)
		d := h.newDevice()
		if rec := h.do(http.MethodPost, "/v1/devices/"+d.id+"/plan", planBody(d, "KEY")); rec.Code != http.StatusServiceUnavailable {
			t.Fatalf("got %d, want 503", rec.Code)
		}
	})
	t.Run("bad inputs", func(t *testing.T) {
		h := newPlanHarness(t, &fakeActivator{lic: ok}, 7)
		d := h.newDevice()
		for name, body := range map[string]any{
			"empty key":      map[string]any{"license_key": "", "signature": planBody(d, "x")["signature"]},
			"padded key":     map[string]any{"license_key": " KEY ", "signature": planBody(d, " KEY ")["signature"]},
			"bad signature":  map[string]any{"license_key": "KEY", "signature": "zz"},
			"unknown fields": map[string]any{"license_key": "KEY", "signature": planBody(d, "KEY")["signature"], "extra": 1},
			"not json":       "{",
		} {
			if rec := h.do(http.MethodPost, "/v1/devices/"+d.id+"/plan", body); rec.Code != http.StatusBadRequest {
				t.Errorf("%s: got %d, want 400 (%s)", name, rec.Code, rec.Body.String())
			}
		}
		if rec := h.do(http.MethodPost, "/v1/devices/not-a-uuid/plan", planBody(d, "KEY")); rec.Code != http.StatusBadRequest {
			t.Errorf("bad uuid: got %d", rec.Code)
		}
		if rec := h.do(http.MethodPost, "/v1/devices/6ba7b810-9dad-11d1-80b4-00c04fd430c8/plan", planBody(d, "KEY")); rec.Code != http.StatusNotFound {
			t.Errorf("unknown device: got %d", rec.Code)
		}
		if rec := h.do(http.MethodGet, "/v1/devices/6ba7b810-9dad-11d1-80b4-00c04fd430c8", nil); rec.Code != http.StatusNotFound {
			t.Errorf("get unknown device: got %d", rec.Code)
		}
	})
}

func mustUUID(t *testing.T, s string) uuid.UUID {
	t.Helper()
	id, err := uuid.Parse(s)
	if err != nil {
		t.Fatal(err)
	}
	return id
}
