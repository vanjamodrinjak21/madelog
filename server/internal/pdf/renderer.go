// Package pdf turns a published page into a PDF by handing its HTML to a
// browser engine that lives in another container. The API server stays a
// static Go binary; Chromium stays where a crash cannot take the API with it.
package pdf

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"io"
	"mime/multipart"
	"net/http"
	"strings"
	"time"
)

// Renderer produces a PDF from a self-contained HTML document.
type Renderer interface {
	Render(ctx context.Context, html []byte) ([]byte, error)
}

// Gotenberg renders through a Gotenberg instance
// (https://gotenberg.dev, the `chromium/convert/html` route).
type Gotenberg struct {
	base   string
	client *http.Client
}

// NewGotenberg points at a Gotenberg origin such as
// http://madelog-pdf.railway.internal:3000.
func NewGotenberg(base string, timeout time.Duration) *Gotenberg {
	return &Gotenberg{
		base:   strings.TrimRight(base, "/"),
		client: &http.Client{Timeout: timeout},
	}
}

// A4 in inches, which is the unit Gotenberg's paper fields take. The page's
// own `@page` rule sets the margins, hence preferCssPageSize and zero margins
// here.
const (
	paperWidth  = "8.27"
	paperHeight = "11.69"
)

// Render implements Renderer.
func (g *Gotenberg) Render(ctx context.Context, html []byte) ([]byte, error) {
	var body bytes.Buffer
	mw := multipart.NewWriter(&body)
	part, err := mw.CreateFormFile("files", "index.html")
	if err != nil {
		return nil, fmt.Errorf("pdf: building request: %w", err)
	}
	if _, err := part.Write(html); err != nil {
		return nil, fmt.Errorf("pdf: building request: %w", err)
	}
	for k, v := range map[string]string{
		"paperWidth":        paperWidth,
		"paperHeight":       paperHeight,
		"marginTop":         "0",
		"marginBottom":      "0",
		"marginLeft":        "0",
		"marginRight":       "0",
		"printBackground":   "true",
		"preferCssPageSize": "true",
	} {
		if err := mw.WriteField(k, v); err != nil {
			return nil, fmt.Errorf("pdf: building request: %w", err)
		}
	}
	if err := mw.Close(); err != nil {
		return nil, fmt.Errorf("pdf: building request: %w", err)
	}

	req, err := http.NewRequestWithContext(ctx, http.MethodPost, g.base+"/forms/chromium/convert/html", &body)
	if err != nil {
		return nil, fmt.Errorf("pdf: building request: %w", err)
	}
	req.Header.Set("Content-Type", mw.FormDataContentType())

	resp, err := g.client.Do(req)
	if err != nil {
		return nil, fmt.Errorf("pdf: renderer unreachable: %w", err)
	}
	defer resp.Body.Close()
	out, err := io.ReadAll(io.LimitReader(resp.Body, 64<<20))
	if err != nil {
		return nil, fmt.Errorf("pdf: reading renderer response: %w", err)
	}
	if resp.StatusCode != http.StatusOK {
		msg := strings.TrimSpace(string(out))
		if len(msg) > 200 {
			msg = msg[:200]
		}
		return nil, fmt.Errorf("pdf: renderer answered %d: %s", resp.StatusCode, msg)
	}
	if !bytes.HasPrefix(out, []byte("%PDF")) {
		return nil, errors.New("pdf: renderer did not return a PDF")
	}
	return out, nil
}
