package store_test

import (
	"context"
	"log/slog"
	"os"
	"testing"
	"time"

	"github.com/jackc/pgx/v5/pgxpool"

	"github.com/madelog/server/internal/migrate"
	"github.com/madelog/server/internal/store"
	"github.com/madelog/server/internal/store/storetest"
)

// TestPostgresStoreConformance runs the same suite as the in-memory fake
// against a real database. It only runs when DATABASE_URL is set:
//
//	make db-up && DATABASE_URL=postgres://madelog:madelog@localhost:5432/madelog?sslmode=disable go test ./...
func TestPostgresStoreConformance(t *testing.T) {
	dsn := os.Getenv("DATABASE_URL")
	if dsn == "" {
		t.Skip("DATABASE_URL is not set; skipping Postgres integration tests")
	}

	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()

	pool, err := pgxpool.New(ctx, dsn)
	if err != nil {
		t.Fatalf("connecting to DATABASE_URL: %v", err)
	}
	t.Cleanup(pool.Close)
	if err := pool.Ping(ctx); err != nil {
		t.Fatalf("pinging DATABASE_URL: %v", err)
	}
	if err := migrate.Apply(ctx, pool, slog.New(slog.DiscardHandler)); err != nil {
		t.Fatalf("applying migrations: %v", err)
	}

	storetest.Run(t, func(t *testing.T) store.Store {
		// Each subtest starts from an empty database.
		if _, err := pool.Exec(context.Background(),
			"TRUNCATE roots, pages, devices, billing_events RESTART IDENTITY CASCADE"); err != nil {
			t.Fatalf("truncating: %v", err)
		}
		return store.FromPool(pool)
	})
}

// TestMigrationsAreIdempotent proves a restart does not try to re-apply.
func TestMigrationsAreIdempotent(t *testing.T) {
	dsn := os.Getenv("DATABASE_URL")
	if dsn == "" {
		t.Skip("DATABASE_URL is not set; skipping Postgres integration tests")
	}
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()

	pool, err := pgxpool.New(ctx, dsn)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(pool.Close)

	log := slog.New(slog.DiscardHandler)
	for i := 0; i < 3; i++ {
		if err := migrate.Apply(ctx, pool, log); err != nil {
			t.Fatalf("apply %d: %v", i, err)
		}
	}
	var n int
	if err := pool.QueryRow(ctx, "SELECT count(*) FROM schema_migrations").Scan(&n); err != nil {
		t.Fatal(err)
	}
	embedded, err := migrate.Embedded()
	if err != nil {
		t.Fatal(err)
	}
	if n != len(embedded) {
		t.Fatalf("schema_migrations has %d rows, want %d", n, len(embedded))
	}
}
