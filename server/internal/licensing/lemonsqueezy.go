// Package licensing turns a LemonSqueezy license key into a plan on a device.
//
// LemonSqueezy's license endpoints (`/v1/licenses/activate`,
// `/v1/licenses/validate`) are public: they take the key itself as the
// credential, so the server needs no API key to call them. An activation is
// bound to an "instance", which for madelog is the device id, so one key
// activates a bounded number of machines and LemonSqueezy enforces the count.
package licensing

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"strconv"
	"strings"
	"time"
)

// DefaultBaseURL is where LemonSqueezy answers.
const DefaultBaseURL = "https://api.lemonsqueezy.com"

// ErrRejected means LemonSqueezy answered and said no: unknown key, expired,
// disabled, or out of activations. The message is theirs.
var ErrRejected = errors.New("licensing: rejected")

// License is what an activation tells us about a key.
type License struct {
	// ID is LemonSqueezy's numeric id of the key, as a string.
	ID string
	// Status is "active", "inactive", "expired" or "disabled".
	Status string
	// ExpiresAt is nil for a key that never expires.
	ExpiresAt *time.Time
	// InstanceID is the activation LemonSqueezy created for this device.
	InstanceID string
	StoreID    int64
	ProductID  int64
}

// Activator activates a key for an instance.
type Activator interface {
	Activate(ctx context.Context, key, instance string) (License, error)
}

// LemonSqueezy is the HTTP Activator.
type LemonSqueezy struct {
	base   string
	client *http.Client
}

// New points at a LemonSqueezy origin; empty means DefaultBaseURL.
func New(base string, timeout time.Duration) *LemonSqueezy {
	if strings.TrimSpace(base) == "" {
		base = DefaultBaseURL
	}
	return &LemonSqueezy{base: strings.TrimRight(base, "/"), client: &http.Client{Timeout: timeout}}
}

// The shape of `POST /v1/licenses/activate`, as documented at
// https://docs.lemonsqueezy.com/api/license-api/activate-license-key.
type activateResponse struct {
	Activated  bool    `json:"activated"`
	Error      *string `json:"error"`
	LicenseKey struct {
		ID        json.Number `json:"id"`
		Status    string      `json:"status"`
		ExpiresAt *string     `json:"expires_at"`
	} `json:"license_key"`
	Instance *struct {
		ID string `json:"id"`
	} `json:"instance"`
	Meta struct {
		StoreID   json.Number `json:"store_id"`
		ProductID json.Number `json:"product_id"`
	} `json:"meta"`
}

// Activate implements Activator.
func (l *LemonSqueezy) Activate(ctx context.Context, key, instance string) (License, error) {
	form := url.Values{"license_key": {key}, "instance_name": {instance}}
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, l.base+"/v1/licenses/activate", strings.NewReader(form.Encode()))
	if err != nil {
		return License{}, fmt.Errorf("licensing: building request: %w", err)
	}
	req.Header.Set("Content-Type", "application/x-www-form-urlencoded")
	req.Header.Set("Accept", "application/json")

	resp, err := l.client.Do(req)
	if err != nil {
		return License{}, fmt.Errorf("licensing: lemonsqueezy unreachable: %w", err)
	}
	defer resp.Body.Close()
	raw, err := io.ReadAll(io.LimitReader(resp.Body, 1<<20))
	if err != nil {
		return License{}, fmt.Errorf("licensing: reading response: %w", err)
	}

	var body activateResponse
	if err := json.Unmarshal(raw, &body); err != nil {
		if resp.StatusCode >= 500 {
			return License{}, fmt.Errorf("licensing: lemonsqueezy answered %d", resp.StatusCode)
		}
		return License{}, fmt.Errorf("licensing: lemonsqueezy answered %d with a body that is not JSON", resp.StatusCode)
	}
	// LemonSqueezy answers 400/404 with {"activated":false,"error":"…"}, and
	// 200 with activated:true. Anything else with a 5xx is theirs to fix.
	if !body.Activated {
		msg := "the license key was not accepted"
		if body.Error != nil && strings.TrimSpace(*body.Error) != "" {
			msg = strings.TrimSpace(*body.Error)
		}
		if resp.StatusCode >= 500 {
			return License{}, fmt.Errorf("licensing: lemonsqueezy answered %d: %s", resp.StatusCode, msg)
		}
		return License{}, fmt.Errorf("%w: %s", ErrRejected, msg)
	}

	out := License{
		ID:     body.LicenseKey.ID.String(),
		Status: body.LicenseKey.Status,
	}
	if body.Instance != nil {
		out.InstanceID = body.Instance.ID
	}
	if body.LicenseKey.ExpiresAt != nil && strings.TrimSpace(*body.LicenseKey.ExpiresAt) != "" {
		t, err := time.Parse(time.RFC3339, *body.LicenseKey.ExpiresAt)
		if err != nil {
			return License{}, fmt.Errorf("licensing: expires_at %q is not RFC 3339", *body.LicenseKey.ExpiresAt)
		}
		t = t.UTC()
		out.ExpiresAt = &t
	}
	out.StoreID, _ = strconv.ParseInt(body.Meta.StoreID.String(), 10, 64)
	out.ProductID, _ = strconv.ParseInt(body.Meta.ProductID.String(), 10, 64)
	return out, nil
}
