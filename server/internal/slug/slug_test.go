package slug

import (
	"regexp"
	"testing"
)

var canonical = regexp.MustCompile(`^[a-z]{3,}-[a-z]{3,}-[a-z2-7]{4}$`)

func TestGenerateShape(t *testing.T) {
	for i := 0; i < 200; i++ {
		s, err := Generate()
		if err != nil {
			t.Fatal(err)
		}
		if !canonical.MatchString(s) {
			t.Fatalf("slug %q does not match <adjective>-<noun>-<4 base32 lower>", s)
		}
		if !Valid(s) {
			t.Fatalf("generated slug %q is rejected by Valid", s)
		}
	}
}

func TestGenerateIsRandom(t *testing.T) {
	seen := map[string]bool{}
	const n = 500
	for i := 0; i < n; i++ {
		s, err := Generate()
		if err != nil {
			t.Fatal(err)
		}
		seen[s] = true
	}
	if len(seen) < n-5 {
		t.Fatalf("only %d unique slugs out of %d: entropy is too low", len(seen), n)
	}
}

func TestWordListsAreBigAndClean(t *testing.T) {
	for name, list := range map[string][]string{"adjectives": Adjectives, "nouns": Nouns} {
		if len(list) < 60 {
			t.Errorf("%s has %d words, want at least 60", name, len(list))
		}
		seen := map[string]bool{}
		for _, w := range list {
			if seen[w] {
				t.Errorf("%s contains duplicate %q", name, w)
			}
			seen[w] = true
			if len(w) < 3 {
				t.Errorf("%s word %q is too short", name, w)
			}
			for _, r := range w {
				if r < 'a' || r > 'z' {
					t.Errorf("%s word %q has a non lowercase-ascii rune %q", name, w, r)
				}
			}
		}
	}
}

func TestValid(t *testing.T) {
	ok := []string{"quiet-heron-k7m2", "a-b-cd", "abc", "a1-b2-c3"}
	bad := []string{
		"", "  ", "Quiet-Heron-k7m2", "quiet heron", "quiet--heron-k7m2",
		"-quiet-heron", "quiet-heron-", "../etc/passwd", "quiet/heron",
		"quiet_heron", "ab",
	}
	for _, s := range ok {
		if !Valid(s) {
			t.Errorf("Valid(%q) = false, want true", s)
		}
	}
	for _, s := range bad {
		if Valid(s) {
			t.Errorf("Valid(%q) = true, want false", s)
		}
	}
	long := make([]byte, 65)
	for i := range long {
		long[i] = 'a'
	}
	if Valid(string(long)) {
		t.Error("a 65-character slug must be rejected")
	}
}
