// Command madelog-server stores published madelog pages and their hash-chain roots.
//
// It applies its embedded migrations at startup, serves the API in spec
// section 5, and shuts down gracefully on SIGINT or SIGTERM.
package main

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"net/http"
	"os"
	"os/signal"
	"syscall"
	"time"

	"github.com/madelog/server/internal/config"
	"github.com/madelog/server/internal/httpapi"
	"github.com/madelog/server/internal/migrate"
	"github.com/madelog/server/internal/store"
)

func main() {
	if err := run(); err != nil {
		// The logger may not exist yet, so this one goes straight to stderr.
		fmt.Fprintf(os.Stderr, "madelog-server: %v\n", err)
		os.Exit(1)
	}
}

func run() error {
	cfg, err := config.Load()
	if err != nil {
		return err
	}
	log := cfg.Logger(os.Stdout)
	slog.SetDefault(log)

	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()

	startupCtx, cancel := context.WithTimeout(ctx, 30*time.Second)
	defer cancel()

	pg, err := store.Open(startupCtx, cfg.DatabaseURL)
	if err != nil {
		return err
	}
	defer pg.Close()
	log.Info("connected to postgres")

	if err := migrate.Apply(startupCtx, pg.Pool(), log); err != nil {
		return err
	}
	log.Info("schema is up to date")

	handler, err := httpapi.NewRouter(pg, httpapi.Config{
		PublicBaseURL:      cfg.PublicBaseURL,
		LemonSqueezySecret: cfg.LemonSqueezySecret,
		Logger:             log,
	})
	if err != nil {
		return err
	}
	if cfg.LemonSqueezySecret == "" {
		log.Warn("LEMONSQUEEZY_SECRET is empty: the billing webhook will answer 503")
	}

	srv := &http.Server{
		Addr:              cfg.ListenAddr,
		Handler:           handler,
		ReadHeaderTimeout: 10 * time.Second,
		ReadTimeout:       60 * time.Second,
		WriteTimeout:      60 * time.Second,
		IdleTimeout:       120 * time.Second,
		ErrorLog:          slog.NewLogLogger(log.Handler(), slog.LevelWarn),
	}

	serveErr := make(chan error, 1)
	go func() {
		log.Info("listening", "addr", cfg.ListenAddr, "public_base_url", cfg.PublicBaseURL)
		if err := srv.ListenAndServe(); err != nil && !errors.Is(err, http.ErrServerClosed) {
			serveErr <- err
			return
		}
		serveErr <- nil
	}()

	select {
	case err := <-serveErr:
		return err
	case <-ctx.Done():
		log.Info("shutting down", "grace", cfg.ShutdownTimeout)
	}

	shutdownCtx, cancelShutdown := context.WithTimeout(context.WithoutCancel(ctx), cfg.ShutdownTimeout)
	defer cancelShutdown()
	if err := srv.Shutdown(shutdownCtx); err != nil {
		return fmt.Errorf("graceful shutdown failed: %w", err)
	}
	log.Info("stopped cleanly")
	return <-serveErr
}
