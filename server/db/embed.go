// Package dbassets embeds the SQL migrations so the binary carries its own
// schema and needs no migration tool at deploy time.
//
// The same directory is what sqlc.yaml reads as its schema, so the Go types and
// the applied schema can never disagree.
package dbassets

import "embed"

// Migrations holds db/migrations/*.sql, applied at startup by internal/migrate.
//
//go:embed migrations/*.sql
var Migrations embed.FS

// MigrationsDir is the path inside Migrations that holds the .sql files.
const MigrationsDir = "migrations"
