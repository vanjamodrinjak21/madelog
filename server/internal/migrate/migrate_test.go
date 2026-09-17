package migrate

import (
	"testing"
	"testing/fstest"
)

func TestParseSortsByNumericVersion(t *testing.T) {
	fsys := fstest.MapFS{
		"migrations/0010_tenth.sql":  {Data: []byte("SELECT 10;")},
		"migrations/0002_second.sql": {Data: []byte("SELECT 2;")},
		"migrations/0001_init.sql":   {Data: []byte("SELECT 1;")},
		"migrations/README.md":       {Data: []byte("not a migration")},
	}
	got, err := parse(fsys, "migrations")
	if err != nil {
		t.Fatal(err)
	}
	want := []string{"0001_init", "0002_second", "0010_tenth"}
	if len(got) != len(want) {
		t.Fatalf("got %d migrations, want %d: %+v", len(got), len(want), got)
	}
	for i, m := range got {
		if m.Version != want[i] {
			t.Fatalf("migration %d = %q, want %q", i, m.Version, want[i])
		}
		if m.SQL == "" {
			t.Fatalf("migration %q has no SQL", m.Version)
		}
	}
}

func TestParseRejectsBadNames(t *testing.T) {
	for name, fsys := range map[string]fstest.MapFS{
		"no version prefix": {"migrations/init.sql": {Data: []byte("SELECT 1;")}},
		"empty file":        {"migrations/0001_init.sql": {Data: []byte("   \n")}},
		"duplicate version": {
			"migrations/0001_init.sql":  {Data: []byte("SELECT 1;")},
			"migrations/0001_other.sql": {Data: []byte("SELECT 2;")},
		},
	} {
		if _, err := parse(fsys, "migrations"); err == nil {
			t.Errorf("%s: expected an error", name)
		}
	}
}

func TestParseEmptyDirIsAnError(t *testing.T) {
	if _, err := parse(fstest.MapFS{"migrations/README.md": {Data: []byte("x")}}, "migrations"); err == nil {
		t.Fatal("expected an error when there are no migrations at all")
	}
}

func TestEmbeddedMigrationsParse(t *testing.T) {
	got, err := Embedded()
	if err != nil {
		t.Fatal(err)
	}
	if len(got) == 0 {
		t.Fatal("no embedded migrations: the binary would start against an empty schema")
	}
	if got[0].Version != "0001_init" {
		t.Fatalf("first migration is %q, want 0001_init", got[0].Version)
	}
}
