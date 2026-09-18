# Build the binary against the module's own Go version, then ship it on a
# distroless base: no shell, no package manager, nothing to patch. The result
# is a ~15 MB image that idles around 20 MB of RSS.
FROM golang:1.27-alpine AS build

WORKDIR /src

# Dependencies first, so a source-only change reuses this layer.
COPY go.mod go.sum ./
RUN go mod download

COPY . .

# CGO is off, so the binary is static. The migrations are embedded via
# db/embed.go, which is why nothing else needs to be copied into the runtime
# image.
ENV CGO_ENABLED=0
RUN go build -trimpath -ldflags="-s -w" -o /out/madelog-server ./cmd/madelog-server

FROM gcr.io/distroless/static-debian12:nonroot

COPY --from=build /out/madelog-server /madelog-server

ENV LISTEN_ADDR=:8080
EXPOSE 8080
USER nonroot:nonroot

ENTRYPOINT ["/madelog-server"]
