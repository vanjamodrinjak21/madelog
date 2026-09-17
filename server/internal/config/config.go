// Package config reads the server's environment. Everything the process needs
// is here, validated once at startup, so no handler ever calls os.Getenv.
package config

import (
	"fmt"
	"log/slog"
	"os"
	"strings"
	"time"
)

// Config is the validated environment.
type Config struct {
	// DatabaseURL is the pgx DSN. Required.
	DatabaseURL string
	// ListenAddr is the address the HTTP server binds. Default ":8080".
	ListenAddr string
	// PublicBaseURL is the origin used to build page URLs, without a trailing
	// slash. Required.
	PublicBaseURL string
	// LemonSqueezySecret signs the billing webhook. Empty means the webhook
	// endpoint answers 503.
	LemonSqueezySecret string

	LogLevel  slog.Leveler
	LogFormat string // "json" or "text"

	// ShutdownTimeout is how long in-flight requests get after SIGTERM.
	ShutdownTimeout time.Duration
}

// Load reads the process environment.
func Load() (Config, error) { return load(os.Getenv) }

func load(get func(string) string) (Config, error) {
	cfg := Config{
		DatabaseURL:        strings.TrimSpace(get("DATABASE_URL")),
		ListenAddr:         strings.TrimSpace(get("LISTEN_ADDR")),
		PublicBaseURL:      strings.TrimRight(strings.TrimSpace(get("PUBLIC_BASE_URL")), "/"),
		LemonSqueezySecret: get("LEMONSQUEEZY_SECRET"),
		LogFormat:          strings.ToLower(strings.TrimSpace(get("LOG_FORMAT"))),
		ShutdownTimeout:    20 * time.Second,
	}

	if cfg.DatabaseURL == "" {
		return Config{}, fmt.Errorf("config: DATABASE_URL is required")
	}
	if cfg.PublicBaseURL == "" {
		return Config{}, fmt.Errorf("config: PUBLIC_BASE_URL is required")
	}
	if cfg.ListenAddr == "" {
		cfg.ListenAddr = ":8080"
	}
	if cfg.LogFormat == "" {
		cfg.LogFormat = "json"
	}
	if cfg.LogFormat != "json" && cfg.LogFormat != "text" {
		return Config{}, fmt.Errorf("config: LOG_FORMAT must be json or text, got %q", cfg.LogFormat)
	}

	level, err := parseLevel(get("LOG_LEVEL"))
	if err != nil {
		return Config{}, err
	}
	cfg.LogLevel = level
	return cfg, nil
}

func parseLevel(s string) (slog.Level, error) {
	switch strings.ToLower(strings.TrimSpace(s)) {
	case "", "info":
		return slog.LevelInfo, nil
	case "debug":
		return slog.LevelDebug, nil
	case "warn", "warning":
		return slog.LevelWarn, nil
	case "error":
		return slog.LevelError, nil
	default:
		return 0, fmt.Errorf("config: LOG_LEVEL must be debug, info, warn or error, got %q", s)
	}
}

// Logger builds the process logger from the configuration.
func (c Config) Logger(w *os.File) *slog.Logger {
	opts := &slog.HandlerOptions{Level: c.LogLevel}
	if c.LogFormat == "text" {
		return slog.New(slog.NewTextHandler(w, opts))
	}
	return slog.New(slog.NewJSONHandler(w, opts))
}
