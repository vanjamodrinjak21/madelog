// Package migrate applies the embedded SQL migrations at startup.
//
// It is deliberately tiny: read db/migrations/*.sql in version order, run each
// unapplied one inside a transaction, record it in schema_migrations. No
// external tool, no down migrations. To change the schema, add a new file.
package migrate

import (
	"context"
	"fmt"
	"io/fs"
	"log/slog"
	"path"
	"sort"
	"strings"

	"github.com/jackc/pgx/v5/pgxpool"

	dbassets "github.com/madelog/server/db"
)

// lockID namespaces the Postgres advisory lock that serialises startup
// migrations across instances. Any constant works as long as it is ours.
const lockID int64 = 7_041_990_155_123

// Migration is one .sql file.
type Migration struct {
	Version string // file name without the .sql suffix, e.g. "0001_init"
	SQL     string
}

// Embedded returns the migrations compiled into the binary.
func Embedded() ([]Migration, error) {
	return parse(dbassets.Migrations, dbassets.MigrationsDir)
}

// Apply runs every migration that has not been applied yet. It is safe to call
// from several instances at once and safe to call repeatedly.
func Apply(ctx context.Context, pool *pgxpool.Pool, log *slog.Logger) error {
	migrations, err := Embedded()
	if err != nil {
		return err
	}
	return apply(ctx, pool, migrations, log)
}

func apply(ctx context.Context, pool *pgxpool.Pool, migrations []Migration, log *slog.Logger) error {
	if log == nil {
		log = slog.Default()
	}
	conn, err := pool.Acquire(ctx)
	if err != nil {
		return fmt.Errorf("migrate: acquiring connection: %w", err)
	}
	defer conn.Release()

	// One instance at a time, so two pods starting together do not both try to
	// create the same table.
	if _, err := conn.Exec(ctx, "SELECT pg_advisory_lock($1)", lockID); err != nil {
		return fmt.Errorf("migrate: taking advisory lock: %w", err)
	}
	defer func() {
		if _, err := conn.Exec(context.WithoutCancel(ctx), "SELECT pg_advisory_unlock($1)", lockID); err != nil {
			log.Warn("migrate: releasing advisory lock", "err", err)
		}
	}()

	if _, err := conn.Exec(ctx, `
		CREATE TABLE IF NOT EXISTS schema_migrations (
			version    text PRIMARY KEY,
			applied_at timestamptz NOT NULL DEFAULT now()
		)`); err != nil {
		return fmt.Errorf("migrate: creating schema_migrations: %w", err)
	}

	applied := map[string]bool{}
	rows, err := conn.Query(ctx, "SELECT version FROM schema_migrations")
	if err != nil {
		return fmt.Errorf("migrate: reading schema_migrations: %w", err)
	}
	for rows.Next() {
		var v string
		if err := rows.Scan(&v); err != nil {
			rows.Close()
			return fmt.Errorf("migrate: scanning schema_migrations: %w", err)
		}
		applied[v] = true
	}
	rows.Close()
	if err := rows.Err(); err != nil {
		return fmt.Errorf("migrate: reading schema_migrations: %w", err)
	}

	for _, m := range migrations {
		if applied[m.Version] {
			continue
		}
		log.Info("applying migration", "version", m.Version)

		tx, err := conn.Begin(ctx)
		if err != nil {
			return fmt.Errorf("migrate: begin %s: %w", m.Version, err)
		}
		if _, err := tx.Exec(ctx, m.SQL); err != nil {
			_ = tx.Rollback(ctx)
			return fmt.Errorf("migrate: applying %s: %w", m.Version, err)
		}
		if _, err := tx.Exec(ctx, "INSERT INTO schema_migrations (version) VALUES ($1)", m.Version); err != nil {
			_ = tx.Rollback(ctx)
			return fmt.Errorf("migrate: recording %s: %w", m.Version, err)
		}
		if err := tx.Commit(ctx); err != nil {
			return fmt.Errorf("migrate: commit %s: %w", m.Version, err)
		}
	}
	return nil
}

// parse reads and validates the .sql files in dir.
func parse(fsys fs.FS, dir string) ([]Migration, error) {
	entries, err := fs.ReadDir(fsys, dir)
	if err != nil {
		return nil, fmt.Errorf("migrate: reading %s: %w", dir, err)
	}

	var out []Migration
	seen := map[string]string{} // numeric prefix -> file name
	for _, e := range entries {
		name := e.Name()
		if e.IsDir() || !strings.HasSuffix(name, ".sql") {
			continue
		}
		version := strings.TrimSuffix(name, ".sql")
		prefix, _, ok := strings.Cut(version, "_")
		if !ok || prefix == "" || !allDigits(prefix) {
			return nil, fmt.Errorf("migrate: %q must be named <digits>_<name>.sql", name)
		}
		if other, dup := seen[prefix]; dup {
			return nil, fmt.Errorf("migrate: %q and %q share version %s", other, name, prefix)
		}
		seen[prefix] = name

		body, err := fs.ReadFile(fsys, path.Join(dir, name))
		if err != nil {
			return nil, fmt.Errorf("migrate: reading %s: %w", name, err)
		}
		if strings.TrimSpace(string(body)) == "" {
			return nil, fmt.Errorf("migrate: %q is empty", name)
		}
		out = append(out, Migration{Version: version, SQL: string(body)})
	}
	if len(out) == 0 {
		return nil, fmt.Errorf("migrate: no .sql files in %s", dir)
	}

	sort.Slice(out, func(i, j int) bool {
		pi, _, _ := strings.Cut(out[i].Version, "_")
		pj, _, _ := strings.Cut(out[j].Version, "_")
		if len(pi) != len(pj) {
			return len(pi) < len(pj) // shorter numeric prefix sorts first
		}
		return out[i].Version < out[j].Version
	})
	return out, nil
}

func allDigits(s string) bool {
	for i := 0; i < len(s); i++ {
		if s[i] < '0' || s[i] > '9' {
			return false
		}
	}
	return len(s) > 0
}
