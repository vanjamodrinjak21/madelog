package store

import (
	"context"
	"sync"
	"time"

	"github.com/google/uuid"
)

// Memory is an in-memory Store used by handler tests and by `go run` smoke
// checks without a database. It is safe for concurrent use.
type Memory struct {
	mu       sync.Mutex
	devices  map[uuid.UUID]Device
	byKey    map[string]uuid.UUID
	pages    map[string]Page
	roots    []RootRecord
	billing  [][]byte
	nextRoot int64

	// PingErr, when set, makes Ping fail. Handy for testing /healthz.
	PingErr error
	// Now lets tests control received_at. Defaults to time.Now().UTC().
	Now func() time.Time
}

// NewMemory returns an empty in-memory store.
func NewMemory() *Memory {
	return &Memory{
		devices: map[uuid.UUID]Device{},
		byKey:   map[string]uuid.UUID{},
		pages:   map[string]Page{},
	}
}

func (m *Memory) now() time.Time {
	if m.Now != nil {
		return m.Now().UTC()
	}
	return time.Now().UTC()
}

// Ping implements Store.
func (m *Memory) Ping(context.Context) error { return m.PingErr }

// Close implements Store.
func (m *Memory) Close() {}

// UpsertDevice implements Store.
func (m *Memory) UpsertDevice(_ context.Context, publicKey []byte) (Device, error) {
	m.mu.Lock()
	defer m.mu.Unlock()
	key := string(publicKey)
	if id, ok := m.byKey[key]; ok {
		return m.devices[id], nil
	}
	d := Device{
		ID:        uuid.New(),
		PublicKey: append([]byte(nil), publicKey...),
		CreatedAt: m.now(),
	}
	m.devices[d.ID] = d
	m.byKey[key] = d.ID
	return d, nil
}

// Device implements Store.
func (m *Memory) Device(_ context.Context, id uuid.UUID) (Device, error) {
	m.mu.Lock()
	defer m.mu.Unlock()
	d, ok := m.devices[id]
	if !ok {
		return Device{}, ErrNotFound
	}
	return d, nil
}

// Page implements Store.
func (m *Memory) Page(_ context.Context, slug string) (Page, error) {
	m.mu.Lock()
	defer m.mu.Unlock()
	p, ok := m.pages[slug]
	if !ok {
		return Page{}, ErrNotFound
	}
	return p, nil
}

// PublishPage implements Store.
func (m *Memory) PublishPage(_ context.Context, in PublishPageParams) (Page, error) {
	m.mu.Lock()
	defer m.mu.Unlock()
	now := m.now()
	p := Page{
		Slug:        in.Slug,
		DeviceID:    in.DeviceID,
		HTML:        in.HTML,
		Root:        in.Root,
		FirstTS:     in.FirstTS.UTC(),
		LastTS:      in.LastTS.UTC(),
		PublishedTS: in.PublishedTS.UTC(),
		ReceivedAt:  now,
		DeletedAt:   nil,
	}
	m.pages[in.Slug] = p
	m.nextRoot++
	m.roots = append(m.roots, RootRecord{ID: m.nextRoot, Slug: in.Slug, Root: in.Root, ReceivedAt: now})
	return p, nil
}

// SoftDeletePage implements Store.
func (m *Memory) SoftDeletePage(_ context.Context, slug string) (Page, error) {
	m.mu.Lock()
	defer m.mu.Unlock()
	p, ok := m.pages[slug]
	if !ok {
		return Page{}, ErrNotFound
	}
	if p.DeletedAt == nil {
		t := m.now()
		p.DeletedAt = &t
	}
	p.HTML = ""
	m.pages[slug] = p
	return p, nil
}

// Roots implements Store.
func (m *Memory) Roots(_ context.Context, slug string) ([]RootRecord, error) {
	m.mu.Lock()
	defer m.mu.Unlock()
	var out []RootRecord
	for _, r := range m.roots {
		if r.Slug == slug {
			out = append(out, r)
		}
	}
	return out, nil
}

// InsertBillingEvent implements Store.
func (m *Memory) InsertBillingEvent(_ context.Context, payload []byte) error {
	m.mu.Lock()
	defer m.mu.Unlock()
	m.billing = append(m.billing, append([]byte(nil), payload...))
	return nil
}

// BillingEvents returns the stored raw payloads. Test helper.
func (m *Memory) BillingEvents() [][]byte {
	m.mu.Lock()
	defer m.mu.Unlock()
	return append([][]byte(nil), m.billing...)
}

var _ Store = (*Memory)(nil)
