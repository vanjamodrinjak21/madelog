// Package slug generates and validates the public page identifiers.
//
// Shape: <adjective>-<noun>-<4 characters of lowercase base32, no padding>.
// The suffix carries 20 bits of entropy, which is plenty because collisions
// are resolved by retrying against the store, not by hoping.
package slug

import (
	"crypto/rand"
	"encoding/base32"
	"fmt"
	"strings"
)

// MaxLen is the longest slug the server will accept from a client.
const MaxLen = 64

// MinLen is the shortest slug the server will accept from a client.
const MinLen = 3

const suffixLen = 4

var lowerBase32 = base32.StdEncoding.WithPadding(base32.NoPadding)

// Generate returns a new random slug.
func Generate() (string, error) {
	adj, err := pick(Adjectives)
	if err != nil {
		return "", err
	}
	noun, err := pick(Nouns)
	if err != nil {
		return "", err
	}
	suf, err := suffix()
	if err != nil {
		return "", err
	}
	return adj + "-" + noun + "-" + suf, nil
}

func pick(list []string) (string, error) {
	n, err := randIndex(len(list))
	if err != nil {
		return "", err
	}
	return list[n], nil
}

// randIndex returns a uniform value in [0, n) using rejection sampling, so no
// word is more likely than another.
func randIndex(n int) (int, error) {
	if n <= 0 {
		return 0, fmt.Errorf("slug: empty word list")
	}
	limit := 256 - (256 % n)
	var b [1]byte
	for {
		if _, err := rand.Read(b[:]); err != nil {
			return 0, fmt.Errorf("slug: read random: %w", err)
		}
		if int(b[0]) < limit {
			return int(b[0]) % n, nil
		}
	}
}

func suffix() (string, error) {
	// 3 bytes -> 5 base32 characters; we keep the first 4 (20 bits).
	var b [3]byte
	if _, err := rand.Read(b[:]); err != nil {
		return "", fmt.Errorf("slug: read random: %w", err)
	}
	return strings.ToLower(lowerBase32.EncodeToString(b[:]))[:suffixLen], nil
}

// Valid reports whether s is safe to use as a public slug: 3 to 64 characters
// of lowercase letters, digits and single hyphens, starting and ending on an
// alphanumeric. Slugs are always minted by Generate, but a re-publish sends one
// back to us, so the input still gets checked.
func Valid(s string) bool {
	if len(s) < MinLen || len(s) > MaxLen {
		return false
	}
	prevHyphen := false
	for i := 0; i < len(s); i++ {
		c := s[i]
		switch {
		case c >= 'a' && c <= 'z', c >= '0' && c <= '9':
			prevHyphen = false
		case c == '-':
			if i == 0 || i == len(s)-1 || prevHyphen {
				return false
			}
			prevHyphen = true
		default:
			return false
		}
	}
	return true
}
