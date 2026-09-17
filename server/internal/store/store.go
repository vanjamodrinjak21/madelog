// Package store is the persistence boundary. Handlers depend on the Store
// interface only, so they can be tested against the in-memory fake in this
// package while production runs the Postgres implementation.
package store

import (
	"context"
	"errors"
	"time"

	"github.com/google/uuid"
)

// ErrNotFound is returned when a row does not exist.
var ErrNotFound = errors.New("store: not found")

// Device is a published-from machine, identified by its ed25519 public key.
type Device struct {
	ID        uuid.UUID
	PublicKey []byte
	Email     *string
	CreatedAt time.Time
}

// Page is a published project page. A deleted page keeps its row (and its
// roots history) but loses its html; DeletedAt is then non-nil.
type Page struct {
	Slug        string
	DeviceID    uuid.UUID
	HTML        string
	Root        string
	FirstTS     time.Time
	LastTS      time.Time
	PublishedTS time.Time
	ReceivedAt  time.Time
	DeletedAt   *time.Time
}

// Deleted reports whether the page has been deleted by its owner.
func (p Page) Deleted() bool { return p.DeletedAt != nil }

// PublishPageParams is the input to PublishPage. ReceivedAt is set by the
// store, never by the client.
type PublishPageParams struct {
	Slug        string
	DeviceID    uuid.UUID
	HTML        string
	Root        string
	FirstTS     time.Time
	LastTS      time.Time
	PublishedTS time.Time
}

// Store is everything the HTTP layer needs from the database.
type Store interface {
	// Ping checks that the database is reachable.
	Ping(ctx context.Context) error

	// UpsertDevice registers a public key, or returns the existing device for
	// it. It is idempotent on the public key.
	UpsertDevice(ctx context.Context, publicKey []byte) (Device, error)

	// Device looks a device up by id. Returns ErrNotFound if unknown.
	Device(ctx context.Context, id uuid.UUID) (Device, error)

	// Page returns the page for a slug, including deleted ones, so callers can
	// tell "gone" apart from "never existed" and so deleted slugs are not
	// handed out again. Returns ErrNotFound if the slug was never used.
	Page(ctx context.Context, slug string) (Page, error)

	// PublishPage inserts or overwrites a page and appends a roots history row
	// in one transaction. Overwriting resurrects a previously deleted page.
	PublishPage(ctx context.Context, p PublishPageParams) (Page, error)

	// SoftDeletePage clears the html and stamps deleted_at, keeping the root
	// and the roots history. Returns ErrNotFound if the slug was never used.
	SoftDeletePage(ctx context.Context, slug string) (Page, error)

	// Roots returns the root history for a slug, oldest first.
	Roots(ctx context.Context, slug string) ([]RootRecord, error)

	// InsertBillingEvent stores a raw, already-verified webhook payload.
	InsertBillingEvent(ctx context.Context, payload []byte) error

	// Close releases resources. Safe to call on a nil-ish store.
	Close()
}

// RootRecord is one entry in a slug's chain-root history.
type RootRecord struct {
	ID         int64
	Slug       string
	Root       string
	ReceivedAt time.Time
}
