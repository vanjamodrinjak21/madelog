# madelog-server
#
#   make run     start the server against DATABASE_URL (see .env.example)
#   make test    go test ./... (DB tests skip unless DATABASE_URL is set)
#   make sqlc    regenerate internal/db from db/queries and db/migrations
#   make lint    gofmt check plus go vet

GO      ?= go
SQLC    ?= sqlc
DB_DSN  ?= postgres://madelog:madelog@localhost:5432/madelog?sslmode=disable

.PHONY: run test test-db sqlc lint fmt build tidy db-up db-down up down clean help

## run: start the server locally, reading .env when it exists
run:
	@set -a; [ -f .env ] && . ./.env; set +a; \
	export DATABASE_URL="$${DATABASE_URL:-$(DB_DSN)}"; \
	export PUBLIC_BASE_URL="$${PUBLIC_BASE_URL:-http://localhost:8080}"; \
	export LOG_FORMAT="$${LOG_FORMAT:-text}"; \
	$(GO) run ./cmd/madelog-server

## test: unit and handler tests; database tests skip without DATABASE_URL
test:
	$(GO) test ./...

## test-db: start Postgres, run the whole suite against it, leave the db running
test-db: db-up
	DATABASE_URL="$(DB_DSN)" $(GO) test ./...

## sqlc: regenerate the type-safe query layer
sqlc:
	$(SQLC) generate

## lint: formatting and vet
lint:
	@unformatted=$$(gofmt -l . | grep -v '^internal/db/' || true); \
	if [ -n "$$unformatted" ]; then echo "gofmt needed:"; echo "$$unformatted"; exit 1; fi
	$(GO) vet ./...

## fmt: rewrite files with gofmt
fmt:
	gofmt -w .

## build: compile the binary to ./bin/madelog-server
build:
	$(GO) build -trimpath -o bin/madelog-server ./cmd/madelog-server

## tidy: prune go.mod and go.sum
tidy:
	$(GO) mod tidy

## db-up: start only Postgres and wait for it
db-up:
	docker compose up -d db
	@printf 'waiting for postgres'
	@for i in $$(seq 1 30); do \
		if docker compose exec -T db pg_isready -U madelog -d madelog >/dev/null 2>&1; then echo ' ready'; exit 0; fi; \
		printf '.'; sleep 1; \
	done; echo ' timed out'; exit 1

## db-down: stop Postgres, keep the volume
db-down:
	docker compose stop db

## up: database and server
up:
	docker compose up --build -d

## down: stop everything and drop the volume
down:
	docker compose down -v

## clean: remove build output
clean:
	rm -rf bin

## help: list targets
help:
	@grep -E '^## ' $(MAKEFILE_LIST) | sed 's/^## //'
