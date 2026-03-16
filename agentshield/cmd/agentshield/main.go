package main

import (
	"embed"
	"flag"
	"fmt"
	"io/fs"
	"log/slog"
	"net/http"
	"os"

	"github.com/agentshield/agentshield/internal/adapter"
	"github.com/agentshield/agentshield/internal/api"
	"github.com/agentshield/agentshield/internal/policy"
	"github.com/agentshield/agentshield/internal/store"
)

//go:embed all:static
var staticFS embed.FS

func main() {
	port := flag.Int("port", 8080, "server port")
	dbPath := flag.String("db", "agentshield.db", "SQLite database path")
	flag.Parse()

	logger := slog.New(slog.NewJSONHandler(os.Stdout, &slog.HandlerOptions{Level: slog.LevelInfo}))

	s, err := store.New(*dbPath)
	if err != nil {
		logger.Error("failed to open store", "error", err)
		os.Exit(1)
	}
	defer s.Close()

	engine, err := policy.NewEngine(s)
	if err != nil {
		logger.Error("failed to create policy engine", "error", err)
		os.Exit(1)
	}

	adapters := adapter.NewRegistry()
	handler := api.NewHandler(s, engine, adapters, logger)

	mux := http.NewServeMux()
	handler.RegisterRoutes(mux)

	// Serve embedded static files for the dashboard.
	staticSub, _ := fs.Sub(staticFS, "static")
	mux.Handle("GET /", http.FileServer(http.FS(staticSub)))

	srv := api.CORS(api.Logging(logger, mux))

	addr := fmt.Sprintf(":%d", *port)
	logger.Info("AgentShield starting", "addr", addr)
	if err := http.ListenAndServe(addr, srv); err != nil {
		logger.Error("server error", "error", err)
		os.Exit(1)
	}
}
