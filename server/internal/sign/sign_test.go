package sign

import (
	"crypto/ed25519"
	"encoding/hex"
	"strings"
	"testing"
)

func TestPublishMessageByteString(t *testing.T) {
	got := PublishMessage("abc123", "2026-09-17T14:02:00Z", "quiet-heron-k7m2")
	want := "abc123\n2026-09-17T14:02:00Z\nquiet-heron-k7m2"
	if string(got) != want {
		t.Fatalf("PublishMessage = %q, want %q", got, want)
	}
}

func TestPublishMessageEmptySlug(t *testing.T) {
	got := PublishMessage("abc123", "2026-09-17T14:02:00Z", "")
	want := "abc123\n2026-09-17T14:02:00Z\n"
	if string(got) != want {
		t.Fatalf("PublishMessage = %q, want %q", got, want)
	}
}

func TestDeleteMessageByteString(t *testing.T) {
	got := DeleteMessage("quiet-heron-k7m2")
	want := "delete\nquiet-heron-k7m2"
	if string(got) != want {
		t.Fatalf("DeleteMessage = %q, want %q", got, want)
	}
}

func TestVerifyHexRoundTrip(t *testing.T) {
	pub, priv, err := ed25519.GenerateKey(nil)
	if err != nil {
		t.Fatal(err)
	}
	msg := PublishMessage(strings.Repeat("a", 64), "2026-09-17T14:02:00Z", "slug-x")
	sig := hex.EncodeToString(ed25519.Sign(priv, msg))

	if err := VerifyHex(pub, msg, sig); err != nil {
		t.Fatalf("VerifyHex: %v", err)
	}
	if err := VerifyHex(pub, PublishMessage(strings.Repeat("b", 64), "2026-09-17T14:02:00Z", "slug-x"), sig); err == nil {
		t.Fatal("VerifyHex accepted a signature over a different message")
	}
	if err := VerifyHex(pub, msg, strings.ToUpper(sig)); err != nil {
		t.Fatalf("VerifyHex should accept uppercase hex: %v", err)
	}
	if err := VerifyHex(pub, msg, "zz"); err == nil {
		t.Fatal("VerifyHex accepted malformed hex")
	}
}

func TestParsePublicKeyHex(t *testing.T) {
	pub, _, _ := ed25519.GenerateKey(nil)
	parsed, err := ParsePublicKeyHex(hex.EncodeToString(pub))
	if err != nil {
		t.Fatal(err)
	}
	if !parsed.Equal(pub) {
		t.Fatal("round trip mismatch")
	}
	if _, err := ParsePublicKeyHex(hex.EncodeToString(pub[:31])); err == nil {
		t.Fatal("accepted short key")
	}
	if _, err := ParsePublicKeyHex("nothex"); err == nil {
		t.Fatal("accepted non-hex key")
	}
}

func TestValidRootHex(t *testing.T) {
	if !ValidRootHex(strings.Repeat("0", 64)) {
		t.Fatal("64 zeros should be a valid root")
	}
	if ValidRootHex(strings.Repeat("A", 64)) {
		t.Fatal("uppercase must be rejected: roots are lowercase hex")
	}
	if ValidRootHex(strings.Repeat("0", 63)) {
		t.Fatal("63 chars should be invalid")
	}
}
