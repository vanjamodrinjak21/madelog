package config

import "testing"

func TestLoadDefaults(t *testing.T) {
	env := map[string]string{
		"DATABASE_URL":    "postgres://pop:pop@localhost:5432/pop",
		"PUBLIC_BASE_URL": "https://pop.example/",
	}
	cfg, err := load(func(k string) string { return env[k] })
	if err != nil {
		t.Fatal(err)
	}
	if cfg.ListenAddr != ":8080" {
		t.Errorf("ListenAddr = %q, want :8080", cfg.ListenAddr)
	}
	if cfg.PublicBaseURL != "https://pop.example" {
		t.Errorf("trailing slash not trimmed: %q", cfg.PublicBaseURL)
	}
	if cfg.LemonSqueezySecret != "" {
		t.Errorf("LemonSqueezySecret = %q, want empty", cfg.LemonSqueezySecret)
	}
	if cfg.LogFormat != "json" {
		t.Errorf("LogFormat = %q, want json", cfg.LogFormat)
	}
}

func TestLoadRequiresDatabaseURLAndBaseURL(t *testing.T) {
	cases := map[string]map[string]string{
		"no database url": {"PUBLIC_BASE_URL": "https://pop.example"},
		"no base url":     {"DATABASE_URL": "postgres://x"},
		"nothing":         {},
	}
	for name, env := range cases {
		if _, err := load(func(k string) string { return env[k] }); err == nil {
			t.Errorf("%s: expected an error", name)
		}
	}
}

func TestLoadOverrides(t *testing.T) {
	env := map[string]string{
		"DATABASE_URL":        "postgres://x",
		"PUBLIC_BASE_URL":     "http://localhost:8080",
		"LISTEN_ADDR":         "127.0.0.1:9000",
		"LEMONSQUEEZY_SECRET": "whsec",
		"LOG_LEVEL":           "debug",
		"LOG_FORMAT":          "text",
	}
	cfg, err := load(func(k string) string { return env[k] })
	if err != nil {
		t.Fatal(err)
	}
	if cfg.ListenAddr != "127.0.0.1:9000" || cfg.LemonSqueezySecret != "whsec" || cfg.LogFormat != "text" {
		t.Fatalf("overrides not applied: %+v", cfg)
	}
	if cfg.LogLevel.Level().String() != "DEBUG" {
		t.Fatalf("LogLevel = %v, want DEBUG", cfg.LogLevel)
	}
}

func TestLoadRejectsBadLogSettings(t *testing.T) {
	base := map[string]string{"DATABASE_URL": "postgres://x", "PUBLIC_BASE_URL": "http://localhost"}
	for _, kv := range [][2]string{{"LOG_LEVEL", "loud"}, {"LOG_FORMAT", "xml"}} {
		env := map[string]string{}
		for k, v := range base {
			env[k] = v
		}
		env[kv[0]] = kv[1]
		if _, err := load(func(k string) string { return env[k] }); err == nil {
			t.Errorf("%s=%s: expected an error", kv[0], kv[1])
		}
	}
}
