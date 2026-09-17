// Package storetest holds the behaviour every store.Store must have. The
// in-memory fake and the Postgres implementation run the same suite, so the
// fake used in handler tests cannot quietly drift from the real database.
package storetest

import (
	"context"
	"crypto/ed25519"
	"errors"
	"testing"
	"time"

	"github.com/google/uuid"

	"github.com/pop-dev/popserver/internal/store"
)

// Run executes the conformance suite. newStore must return a store with no
// rows in it; it may call t.Skip.
func Run(t *testing.T, newStore func(t *testing.T) store.Store) {
	t.Helper()

	sub := func(name string, fn func(t *testing.T, s store.Store)) {
		t.Run(name, func(t *testing.T) {
			s := newStore(t)
			fn(t, s)
		})
	}

	sub("Ping", func(t *testing.T, s store.Store) {
		if err := s.Ping(context.Background()); err != nil {
			t.Fatalf("Ping: %v", err)
		}
	})

	sub("UpsertDeviceIsIdempotent", func(t *testing.T, s store.Store) {
		ctx := context.Background()
		pub, _, _ := ed25519.GenerateKey(nil)
		a, err := s.UpsertDevice(ctx, pub)
		if err != nil {
			t.Fatal(err)
		}
		b, err := s.UpsertDevice(ctx, pub)
		if err != nil {
			t.Fatal(err)
		}
		if a.ID != b.ID {
			t.Fatalf("same public key produced two device ids: %s and %s", a.ID, b.ID)
		}
		if string(b.PublicKey) != string(pub) {
			t.Fatal("public key not round-tripped")
		}
		if a.CreatedAt.IsZero() {
			t.Fatal("created_at not set")
		}

		other, _, _ := ed25519.GenerateKey(nil)
		c, err := s.UpsertDevice(ctx, other)
		if err != nil {
			t.Fatal(err)
		}
		if c.ID == a.ID {
			t.Fatal("different public keys shared a device id")
		}

		got, err := s.Device(ctx, a.ID)
		if err != nil {
			t.Fatal(err)
		}
		if got.ID != a.ID {
			t.Fatal("Device returned the wrong row")
		}
		if _, err := s.Device(ctx, uuid.New()); !errors.Is(err, store.ErrNotFound) {
			t.Fatalf("Device(unknown) = %v, want ErrNotFound", err)
		}
	})

	sub("PublishPageInsertsAndOverwrites", func(t *testing.T, s store.Store) {
		ctx := context.Background()
		dev := mustDevice(t, s)

		first := time.Date(2026, 9, 3, 9, 12, 0, 0, time.UTC)
		last := time.Date(2026, 9, 17, 14, 2, 0, 0, time.UTC)
		pub := time.Date(2026, 9, 17, 14, 5, 0, 0, time.UTC)

		p, err := s.PublishPage(ctx, store.PublishPageParams{
			Slug: "quiet-heron-k7m2", DeviceID: dev.ID, HTML: "<h1>one</h1>",
			Root: rootA, FirstTS: first, LastTS: last, PublishedTS: pub,
		})
		if err != nil {
			t.Fatal(err)
		}
		if p.ReceivedAt.IsZero() {
			t.Fatal("received_at not set by the server")
		}
		if p.Deleted() {
			t.Fatal("new page should not be deleted")
		}

		got, err := s.Page(ctx, "quiet-heron-k7m2")
		if err != nil {
			t.Fatal(err)
		}
		if got.HTML != "<h1>one</h1>" || got.Root != rootA {
			t.Fatalf("round trip mismatch: %+v", got)
		}
		if !got.FirstTS.Equal(first) || !got.LastTS.Equal(last) || !got.PublishedTS.Equal(pub) {
			t.Fatalf("timestamps not round-tripped: %+v", got)
		}

		// Re-publishing the same device+slug overwrites html and root.
		if _, err := s.PublishPage(ctx, store.PublishPageParams{
			Slug: "quiet-heron-k7m2", DeviceID: dev.ID, HTML: "<h1>two</h1>",
			Root: rootB, FirstTS: first, LastTS: last.Add(time.Hour), PublishedTS: pub.Add(time.Hour),
		}); err != nil {
			t.Fatal(err)
		}
		got, err = s.Page(ctx, "quiet-heron-k7m2")
		if err != nil {
			t.Fatal(err)
		}
		if got.HTML != "<h1>two</h1>" || got.Root != rootB {
			t.Fatalf("re-publish did not overwrite: %+v", got)
		}

		// ...and keeps a roots history row per publish.
		roots, err := s.Roots(ctx, "quiet-heron-k7m2")
		if err != nil {
			t.Fatal(err)
		}
		if len(roots) != 2 {
			t.Fatalf("got %d roots rows, want 2", len(roots))
		}
		if roots[0].Root != rootA || roots[1].Root != rootB {
			t.Fatalf("roots history out of order: %+v", roots)
		}

		if _, err := s.Page(ctx, "never-published-aaaa"); !errors.Is(err, store.ErrNotFound) {
			t.Fatalf("Page(unknown) = %v, want ErrNotFound", err)
		}
	})

	sub("SoftDeleteKeepsRootDropsHTML", func(t *testing.T, s store.Store) {
		ctx := context.Background()
		dev := mustDevice(t, s)
		now := time.Now().UTC().Truncate(time.Second)
		if _, err := s.PublishPage(ctx, store.PublishPageParams{
			Slug: "tidy-otter-b3ka", DeviceID: dev.ID, HTML: "<h1>bye</h1>",
			Root: rootA, FirstTS: now, LastTS: now, PublishedTS: now,
		}); err != nil {
			t.Fatal(err)
		}

		del, err := s.SoftDeletePage(ctx, "tidy-otter-b3ka")
		if err != nil {
			t.Fatal(err)
		}
		if !del.Deleted() {
			t.Fatal("deleted_at not stamped")
		}
		if del.HTML != "" {
			t.Fatal("html not cleared")
		}
		if del.Root != rootA {
			t.Fatal("root must survive deletion")
		}

		got, err := s.Page(ctx, "tidy-otter-b3ka")
		if err != nil {
			t.Fatalf("a deleted page must still be found by slug, got %v", err)
		}
		if !got.Deleted() || got.HTML != "" || got.Root != rootA {
			t.Fatalf("unexpected state after delete: %+v", got)
		}

		roots, err := s.Roots(ctx, "tidy-otter-b3ka")
		if err != nil {
			t.Fatal(err)
		}
		if len(roots) != 1 {
			t.Fatalf("delete dropped the roots history: %d rows", len(roots))
		}

		// Re-publishing brings it back.
		if _, err := s.PublishPage(ctx, store.PublishPageParams{
			Slug: "tidy-otter-b3ka", DeviceID: dev.ID, HTML: "<h1>back</h1>",
			Root: rootB, FirstTS: now, LastTS: now, PublishedTS: now,
		}); err != nil {
			t.Fatal(err)
		}
		got, _ = s.Page(ctx, "tidy-otter-b3ka")
		if got.Deleted() || got.HTML != "<h1>back</h1>" {
			t.Fatalf("re-publish did not undelete: %+v", got)
		}

		if _, err := s.SoftDeletePage(ctx, "no-such-slug-aa"); !errors.Is(err, store.ErrNotFound) {
			t.Fatalf("SoftDeletePage(unknown) = %v, want ErrNotFound", err)
		}
	})

	sub("InsertBillingEvent", func(t *testing.T, s store.Store) {
		ctx := context.Background()
		if err := s.InsertBillingEvent(ctx, []byte(`{"meta":{"event_name":"order_created"}}`)); err != nil {
			t.Fatal(err)
		}
	})
}

const (
	rootA = "0000000000000000000000000000000000000000000000000000000000000001"
	rootB = "0000000000000000000000000000000000000000000000000000000000000002"
)

func mustDevice(t *testing.T, s store.Store) store.Device {
	t.Helper()
	pub, _, err := ed25519.GenerateKey(nil)
	if err != nil {
		t.Fatal(err)
	}
	d, err := s.UpsertDevice(context.Background(), pub)
	if err != nil {
		t.Fatal(err)
	}
	return d
}
