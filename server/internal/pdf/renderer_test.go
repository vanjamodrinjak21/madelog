package pdf

import (
	"context"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"
)

func TestGotenbergSendsTheDocumentAndReadsThePDF(t *testing.T) {
	var gotPath, gotName, gotBody string
	var gotFields map[string]string
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		gotPath = r.URL.Path
		if err := r.ParseMultipartForm(1 << 20); err != nil {
			t.Errorf("parse form: %v", err)
		}
		f, hdr, err := r.FormFile("files")
		if err != nil {
			t.Errorf("no files part: %v", err)
			return
		}
		defer f.Close()
		gotName = hdr.Filename
		b, _ := io.ReadAll(f)
		gotBody = string(b)
		gotFields = map[string]string{}
		for k := range r.MultipartForm.Value {
			gotFields[k] = r.FormValue(k)
		}
		w.Header().Set("Content-Type", "application/pdf")
		_, _ = w.Write([]byte("%PDF-1.7\nok"))
	}))
	defer srv.Close()

	out, err := NewGotenberg(srv.URL+"/", time.Second).Render(context.Background(), []byte("<h1>x</h1>"))
	if err != nil {
		t.Fatal(err)
	}
	if string(out) != "%PDF-1.7\nok" {
		t.Fatalf("pdf = %q", out)
	}
	if gotPath != "/forms/chromium/convert/html" {
		t.Fatalf("path = %q", gotPath)
	}
	if gotName != "index.html" || gotBody != "<h1>x</h1>" {
		t.Fatalf("file = %q %q", gotName, gotBody)
	}
	for k, want := range map[string]string{
		"paperWidth": "8.27", "paperHeight": "11.69", "printBackground": "true", "preferCssPageSize": "true", "marginTop": "0",
	} {
		if gotFields[k] != want {
			t.Errorf("field %s = %q, want %q", k, gotFields[k], want)
		}
	}
}

func TestGotenbergRejectsNonPDFAndErrors(t *testing.T) {
	t.Run("non-200", func(t *testing.T) {
		srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			http.Error(w, "chromium crashed", http.StatusInternalServerError)
		}))
		defer srv.Close()
		_, err := NewGotenberg(srv.URL, time.Second).Render(context.Background(), []byte("x"))
		if err == nil || !strings.Contains(err.Error(), "500") || !strings.Contains(err.Error(), "chromium crashed") {
			t.Fatalf("err = %v", err)
		}
	})
	t.Run("not a pdf", func(t *testing.T) {
		srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			_, _ = w.Write([]byte("<html>oops</html>"))
		}))
		defer srv.Close()
		if _, err := NewGotenberg(srv.URL, time.Second).Render(context.Background(), []byte("x")); err == nil {
			t.Fatal("expected an error for a non-PDF body")
		}
	})
	t.Run("unreachable", func(t *testing.T) {
		if _, err := NewGotenberg("http://127.0.0.1:1", 200*time.Millisecond).Render(context.Background(), []byte("x")); err == nil {
			t.Fatal("expected an error when the renderer is down")
		}
	})
}
