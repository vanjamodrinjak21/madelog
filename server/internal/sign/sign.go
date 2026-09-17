// Package sign holds the exact byte strings the pop CLI signs with its device
// ed25519 key, plus the verification helpers the HTTP handlers use.
//
// The byte strings are a wire contract shared with the CLI. They are defined
// here, once, and documented in server/README.md. Do not change them without
// changing the CLI.
package sign

import (
	"crypto/ed25519"
	"encoding/hex"
	"errors"
	"fmt"
)

// PublicKeySize and SignatureSize are the raw byte lengths; on the wire both
// are lowercase hex, so twice as many characters.
const (
	PublicKeySize = ed25519.PublicKeySize // 32
	SignatureSize = ed25519.SignatureSize // 64
	RootHexLen    = 64                    // sha256, lowercase hex
)

var (
	// ErrBadEncoding means the hex was malformed or the wrong length.
	ErrBadEncoding = errors.New("sign: malformed hex encoding")
	// ErrBadSignature means the bytes decoded but the signature did not verify.
	ErrBadSignature = errors.New("sign: signature does not verify")
)

// PublishMessage is the byte string signed by POST /v1/pages.
//
//	root + "\n" + published_ts + "\n" + slug
//
// root is lowercase hex sha256, published_ts is RFC 3339 in UTC (trailing "Z"),
// and slug is exactly what the client put in the request body. When the client
// does not yet have a slug it sends none and signs over an empty slug, so the
// message ends with a trailing newline.
func PublishMessage(root, publishedTS, slug string) []byte {
	return []byte(root + "\n" + publishedTS + "\n" + slug)
}

// DeleteMessage is the byte string signed by DELETE /v1/pages/{slug}.
//
//	"delete" + "\n" + slug
func DeleteMessage(slug string) []byte {
	return []byte("delete\n" + slug)
}

// ParsePublicKeyHex decodes a hex-encoded 32-byte ed25519 public key.
func ParsePublicKeyHex(s string) (ed25519.PublicKey, error) {
	b, err := hex.DecodeString(s)
	if err != nil {
		return nil, fmt.Errorf("%w: public key: %v", ErrBadEncoding, err)
	}
	if len(b) != PublicKeySize {
		return nil, fmt.Errorf("%w: public key must be %d bytes, got %d", ErrBadEncoding, PublicKeySize, len(b))
	}
	return ed25519.PublicKey(b), nil
}

// ParseSignatureHex decodes a hex-encoded 64-byte ed25519 signature.
func ParseSignatureHex(s string) ([]byte, error) {
	b, err := hex.DecodeString(s)
	if err != nil {
		return nil, fmt.Errorf("%w: signature: %v", ErrBadEncoding, err)
	}
	if len(b) != SignatureSize {
		return nil, fmt.Errorf("%w: signature must be %d bytes, got %d", ErrBadEncoding, SignatureSize, len(b))
	}
	return b, nil
}

// VerifyHex checks a hex-encoded signature over msg against pub.
func VerifyHex(pub ed25519.PublicKey, msg []byte, sigHex string) error {
	sig, err := ParseSignatureHex(sigHex)
	if err != nil {
		return err
	}
	if len(pub) != PublicKeySize {
		return fmt.Errorf("%w: stored public key has %d bytes", ErrBadEncoding, len(pub))
	}
	if !ed25519.Verify(pub, msg, sig) {
		return ErrBadSignature
	}
	return nil
}

// ValidRootHex reports whether s is 64 lowercase hex characters.
func ValidRootHex(s string) bool {
	if len(s) != RootHexLen {
		return false
	}
	for i := 0; i < len(s); i++ {
		c := s[i]
		if (c < '0' || c > '9') && (c < 'a' || c > 'f') {
			return false
		}
	}
	return true
}
