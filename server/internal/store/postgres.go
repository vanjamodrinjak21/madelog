package store

import (
	"context"
	"errors"
	"fmt"
	"time"

	"github.com/google/uuid"
	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgxpool"

	"github.com/madelog/server/internal/db"
)

// Postgres is the production Store, backed by a pgx connection pool and the
// sqlc-generated queries in internal/db.
type Postgres struct {
	pool *pgxpool.Pool
	q    *db.Queries
}

// Open dials DATABASE_URL and returns a ready pool. It pings once so a bad DSN
// fails at startup rather than on the first request.
func Open(ctx context.Context, dsn string) (*Postgres, error) {
	cfg, err := pgxpool.ParseConfig(dsn)
	if err != nil {
		return nil, fmt.Errorf("store: parsing DATABASE_URL: %w", err)
	}
	pool, err := pgxpool.NewWithConfig(ctx, cfg)
	if err != nil {
		return nil, fmt.Errorf("store: connecting: %w", err)
	}
	if err := pool.Ping(ctx); err != nil {
		pool.Close()
		return nil, fmt.Errorf("store: pinging: %w", err)
	}
	return FromPool(pool), nil
}

// FromPool wraps an existing pool, for tests and for the migrator, which needs
// the same connection.
func FromPool(pool *pgxpool.Pool) *Postgres {
	return &Postgres{pool: pool, q: db.New(pool)}
}

// Pool exposes the underlying pool so the migrator can reuse it.
func (p *Postgres) Pool() *pgxpool.Pool { return p.pool }

// Ping implements Store.
func (p *Postgres) Ping(ctx context.Context) error { return p.pool.Ping(ctx) }

// Close implements Store.
func (p *Postgres) Close() {
	if p != nil && p.pool != nil {
		p.pool.Close()
	}
}

// UpsertDevice implements Store.
func (p *Postgres) UpsertDevice(ctx context.Context, publicKey []byte) (Device, error) {
	row, err := p.q.UpsertDevice(ctx, publicKey)
	if err != nil {
		return Device{}, fmt.Errorf("store: upsert device: %w", err)
	}
	return deviceFromRow(row), nil
}

// Device implements Store.
func (p *Postgres) Device(ctx context.Context, id uuid.UUID) (Device, error) {
	row, err := p.q.GetDevice(ctx, id)
	if errors.Is(err, pgx.ErrNoRows) {
		return Device{}, ErrNotFound
	}
	if err != nil {
		return Device{}, fmt.Errorf("store: get device: %w", err)
	}
	return deviceFromRow(row), nil
}

// SetDevicePlan implements Store.
func (p *Postgres) SetDevicePlan(ctx context.Context, id uuid.UUID, plan string, source *string, until *time.Time) (Device, error) {
	row, err := p.q.SetDevicePlan(ctx, db.SetDevicePlanParams{ID: id, Plan: plan, PlanSource: source, PlanUntil: until})
	if errors.Is(err, pgx.ErrNoRows) {
		return Device{}, ErrNotFound
	}
	if err != nil {
		return Device{}, fmt.Errorf("store: set device plan: %w", err)
	}
	return deviceFromRow(row), nil
}

// PagePDF implements Store.
func (p *Postgres) PagePDF(ctx context.Context, slug, root, stampDay string) (PagePDF, error) {
	row, err := p.q.GetPagePDF(ctx, db.GetPagePDFParams{Slug: slug, Root: root, StampDay: stampDay})
	if errors.Is(err, pgx.ErrNoRows) {
		return PagePDF{}, ErrNotFound
	}
	if err != nil {
		return PagePDF{}, fmt.Errorf("store: get page pdf: %w", err)
	}
	return PagePDF{Slug: row.Slug, Root: row.Root, StampDay: row.StampDay, PDF: row.Pdf, RenderedAt: row.RenderedAt.UTC()}, nil
}

// PutPagePDF implements Store. The stale-root sweep and the insert share a
// transaction so a reader never sees the slug with no PDF at all.
func (p *Postgres) PutPagePDF(ctx context.Context, slug, root, stampDay string, pdf []byte) error {
	tx, err := p.pool.Begin(ctx)
	if err != nil {
		return fmt.Errorf("store: begin: %w", err)
	}
	defer func() { _ = tx.Rollback(ctx) }()
	q := p.q.WithTx(tx)
	if err := q.DeleteStalePagePDFs(ctx, db.DeleteStalePagePDFsParams{Slug: slug, Root: root}); err != nil {
		return fmt.Errorf("store: delete stale pdfs: %w", err)
	}
	if err := q.UpsertPagePDF(ctx, db.UpsertPagePDFParams{Slug: slug, Root: root, StampDay: stampDay, Pdf: pdf}); err != nil {
		return fmt.Errorf("store: upsert pdf: %w", err)
	}
	if err := tx.Commit(ctx); err != nil {
		return fmt.Errorf("store: commit: %w", err)
	}
	return nil
}

// Page implements Store.
func (p *Postgres) Page(ctx context.Context, slug string) (Page, error) {
	row, err := p.q.GetPage(ctx, slug)
	if errors.Is(err, pgx.ErrNoRows) {
		return Page{}, ErrNotFound
	}
	if err != nil {
		return Page{}, fmt.Errorf("store: get page: %w", err)
	}
	return pageFromRow(row), nil
}

// PublishPage implements Store. The page upsert and the roots history row go
// in one transaction: a published page always has a matching root record.
func (p *Postgres) PublishPage(ctx context.Context, in PublishPageParams) (Page, error) {
	tx, err := p.pool.Begin(ctx)
	if err != nil {
		return Page{}, fmt.Errorf("store: begin: %w", err)
	}
	defer func() { _ = tx.Rollback(ctx) }()

	q := p.q.WithTx(tx)
	row, err := q.UpsertPage(ctx, db.UpsertPageParams{
		Slug:        in.Slug,
		DeviceID:    in.DeviceID,
		Html:        in.HTML,
		Root:        in.Root,
		FirstTs:     in.FirstTS,
		LastTs:      in.LastTS,
		PublishedTs: in.PublishedTS,
	})
	if err != nil {
		return Page{}, fmt.Errorf("store: upsert page: %w", err)
	}
	if err := q.InsertRoot(ctx, db.InsertRootParams{Slug: in.Slug, Root: in.Root}); err != nil {
		return Page{}, fmt.Errorf("store: insert root: %w", err)
	}
	if err := tx.Commit(ctx); err != nil {
		return Page{}, fmt.Errorf("store: commit: %w", err)
	}
	return pageFromRow(row), nil
}

// SoftDeletePage implements Store.
func (p *Postgres) SoftDeletePage(ctx context.Context, slug string) (Page, error) {
	row, err := p.q.SoftDeletePage(ctx, slug)
	if errors.Is(err, pgx.ErrNoRows) {
		return Page{}, ErrNotFound
	}
	if err != nil {
		return Page{}, fmt.Errorf("store: soft delete page: %w", err)
	}
	return pageFromRow(row), nil
}

// Roots implements Store.
func (p *Postgres) Roots(ctx context.Context, slug string) ([]RootRecord, error) {
	rows, err := p.q.ListRoots(ctx, slug)
	if err != nil {
		return nil, fmt.Errorf("store: list roots: %w", err)
	}
	out := make([]RootRecord, 0, len(rows))
	for _, r := range rows {
		out = append(out, RootRecord{ID: r.ID, Slug: r.Slug, Root: r.Root, ReceivedAt: r.ReceivedAt.UTC()})
	}
	return out, nil
}

// InsertBillingEvent implements Store.
func (p *Postgres) InsertBillingEvent(ctx context.Context, payload []byte) error {
	if err := p.q.InsertBillingEvent(ctx, payload); err != nil {
		return fmt.Errorf("store: insert billing event: %w", err)
	}
	return nil
}

func deviceFromRow(d db.Device) Device {
	out := Device{
		ID:         d.ID,
		PublicKey:  d.PublicKey,
		Email:      d.Email,
		CreatedAt:  d.CreatedAt.UTC(),
		Plan:       d.Plan,
		PlanSource: d.PlanSource,
	}
	if d.PlanUntil != nil {
		t := d.PlanUntil.UTC()
		out.PlanUntil = &t
	}
	return out
}

func pageFromRow(p db.Page) Page {
	out := Page{
		Slug:        p.Slug,
		DeviceID:    p.DeviceID,
		HTML:        p.Html,
		Root:        p.Root,
		FirstTS:     p.FirstTs.UTC(),
		LastTS:      p.LastTs.UTC(),
		PublishedTS: p.PublishedTs.UTC(),
		ReceivedAt:  p.ReceivedAt.UTC(),
	}
	if p.DeletedAt != nil {
		t := p.DeletedAt.UTC()
		out.DeletedAt = &t
	}
	return out
}

var _ Store = (*Postgres)(nil)
