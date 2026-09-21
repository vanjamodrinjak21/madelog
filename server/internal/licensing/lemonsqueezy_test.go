package licensing

import (
	"context"
	"errors"
	"net/http"
	"net/http/httptest"
	"testing"
	"time"
)

func TestActivateParsesASuccess(t *testing.T) {
	var gotKey, gotInstance, gotPath string
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		gotPath = r.URL.Path
		_ = r.ParseForm()
		gotKey, gotInstance = r.PostFormValue("license_key"), r.PostFormValue("instance_name")
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(`{"activated":true,"error":null,
		  "license_key":{"id":1,"status":"active","key":"38b1460a-…","activation_limit":3,"activation_usage":1,"created_at":"2026-09-01T00:00:00.000000Z","expires_at":"2027-09-01T00:00:00.000000Z"},
		  "instance":{"id":"47596ad9-a1e2-4ee5-be40-6be8d7f6c0e8","name":"dev","created_at":"2026-09-21T10:00:00.000000Z"},
		  "meta":{"store_id":7,"order_id":8,"order_item_id":9,"product_id":10,"product_name":"madelog Pro","variant_id":11,"variant_name":"Default","customer_id":12,"customer_name":"A","customer_email":"a@example.test"}}`))
	}))
	defer srv.Close()

	lic, err := New(srv.URL, time.Second).Activate(context.Background(), "KEY-1", "device-uuid")
	if err != nil {
		t.Fatal(err)
	}
	if gotPath != "/v1/licenses/activate" || gotKey != "KEY-1" || gotInstance != "device-uuid" {
		t.Fatalf("request = %s %s %s", gotPath, gotKey, gotInstance)
	}
	if lic.ID != "1" || lic.Status != "active" || lic.InstanceID != "47596ad9-a1e2-4ee5-be40-6be8d7f6c0e8" {
		t.Fatalf("license = %+v", lic)
	}
	if lic.ExpiresAt == nil || !lic.ExpiresAt.Equal(time.Date(2027, 9, 1, 0, 0, 0, 0, time.UTC)) {
		t.Fatalf("expires_at = %v", lic.ExpiresAt)
	}
	if lic.StoreID != 7 || lic.ProductID != 10 {
		t.Fatalf("meta = %+v", lic)
	}
}

func TestActivateWithoutExpiry(t *testing.T) {
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		_, _ = w.Write([]byte(`{"activated":true,"license_key":{"id":2,"status":"active","expires_at":null},"instance":{"id":"i"},"meta":{"store_id":7,"product_id":10}}`))
	}))
	defer srv.Close()
	lic, err := New(srv.URL, time.Second).Activate(context.Background(), "K", "d")
	if err != nil {
		t.Fatal(err)
	}
	if lic.ExpiresAt != nil {
		t.Fatalf("expires_at should be nil, got %v", lic.ExpiresAt)
	}
}

func TestActivateRejections(t *testing.T) {
	cases := map[string]struct {
		status int
		body   string
		wantRe bool
	}{
		"unknown key":     {404, `{"activated":false,"error":"license_key not found","license_key":{},"meta":{}}`, true},
		"limit reached":   {400, `{"activated":false,"error":"This license key has reached the activation limit.","license_key":{"id":1,"status":"active"},"meta":{}}`, true},
		"expired":         {400, `{"activated":false,"error":"This license key has expired.","license_key":{"id":1,"status":"expired"},"meta":{}}`, true},
		"server error":    {500, `{"activated":false,"error":"boom"}`, false},
		"not json at all": {502, `<html>bad gateway</html>`, false},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				w.WriteHeader(tc.status)
				_, _ = w.Write([]byte(tc.body))
			}))
			defer srv.Close()
			_, err := New(srv.URL, time.Second).Activate(context.Background(), "K", "d")
			if err == nil {
				t.Fatal("expected an error")
			}
			if errors.Is(err, ErrRejected) != tc.wantRe {
				t.Fatalf("errors.Is(ErrRejected) = %v, want %v (%v)", !tc.wantRe, tc.wantRe, err)
			}
		})
	}
}
