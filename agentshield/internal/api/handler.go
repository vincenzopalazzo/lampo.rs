package api

import (
	"encoding/json"
	"log/slog"
	"net/http"
	"time"

	"github.com/agentshield/agentshield/internal/adapter"
	"github.com/agentshield/agentshield/internal/model"
	"github.com/agentshield/agentshield/internal/policy"
	"github.com/agentshield/agentshield/internal/store"
	"github.com/google/uuid"
)

// Handler holds dependencies for API endpoints.
type Handler struct {
	store    *store.Store
	engine   *policy.Engine
	adapters *adapter.Registry
	logger   *slog.Logger
}

// NewHandler creates a new API handler.
func NewHandler(s *store.Store, e *policy.Engine, a *adapter.Registry, l *slog.Logger) *Handler {
	return &Handler{store: s, engine: e, adapters: a, logger: l}
}

// RegisterRoutes registers all API routes on the given mux.
func (h *Handler) RegisterRoutes(mux *http.ServeMux) {
	mux.HandleFunc("POST /api/v1/actions", h.submitAction)
	mux.HandleFunc("GET /api/v1/actions", h.listActions)
	mux.HandleFunc("GET /api/v1/actions/{id}", h.getAction)
	mux.HandleFunc("POST /api/v1/actions/{id}/approve", h.approveAction)
	mux.HandleFunc("POST /api/v1/actions/{id}/reject", h.rejectAction)
	mux.HandleFunc("GET /api/v1/policy", h.getPolicy)
	mux.HandleFunc("PATCH /api/v1/policy", h.updatePolicy)
	mux.HandleFunc("POST /api/v1/reset", h.reset)
	mux.HandleFunc("GET /api/v1/health", h.health)
}

func (h *Handler) submitAction(w http.ResponseWriter, r *http.Request) {
	var req model.SubmitActionRequest
	if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
		writeError(w, http.StatusBadRequest, "invalid request body")
		return
	}
	if req.AgentID == "" || req.ActionType == "" {
		writeError(w, http.StatusBadRequest, "agent_id and action_type are required")
		return
	}

	now := time.Now()
	action := &model.ActionRequest{
		ID:         uuid.New().String(),
		AgentID:    req.AgentID,
		ActionType: req.ActionType,
		Parameters: req.Parameters,
		CreatedAt:  now,
		UpdatedAt:  now,
	}

	decision, reason := h.engine.Evaluate(action)
	action.Decision = decision
	action.Reason = reason

	switch decision {
	case model.DecisionAllow:
		result := h.adapters.Execute(action)
		action.Result = result
		if result.Success {
			action.Status = model.StatusExecuted
		} else {
			action.Status = model.StatusFailed
		}
	case model.DecisionDeny:
		action.Status = model.StatusDenied
	case model.DecisionPending:
		action.Status = model.StatusPending
	}

	if err := h.store.SaveAction(action); err != nil {
		h.logger.Error("failed to save action", "error", err)
		writeError(w, http.StatusInternalServerError, "failed to save action")
		return
	}

	h.logger.Info("action processed",
		"request_id", action.ID,
		"agent_id", action.AgentID,
		"action_type", action.ActionType,
		"decision", action.Decision,
		"status", action.Status,
	)

	writeJSON(w, http.StatusOK, action)
}

func (h *Handler) getAction(w http.ResponseWriter, r *http.Request) {
	id := r.PathValue("id")
	action, err := h.store.GetAction(id)
	if err != nil {
		writeError(w, http.StatusNotFound, "action not found")
		return
	}
	writeJSON(w, http.StatusOK, action)
}

func (h *Handler) listActions(w http.ResponseWriter, r *http.Request) {
	status := r.URL.Query().Get("status")
	actions, err := h.store.ListActions(status)
	if err != nil {
		h.logger.Error("failed to list actions", "error", err)
		writeError(w, http.StatusInternalServerError, "failed to list actions")
		return
	}
	if actions == nil {
		actions = []*model.ActionRequest{}
	}
	writeJSON(w, http.StatusOK, actions)
}

func (h *Handler) approveAction(w http.ResponseWriter, r *http.Request) {
	id := r.PathValue("id")
	action, err := h.store.GetAction(id)
	if err != nil {
		writeError(w, http.StatusNotFound, "action not found")
		return
	}
	if action.Status != model.StatusPending {
		writeError(w, http.StatusConflict, "action is not pending")
		return
	}

	result := h.adapters.Execute(action)
	action.Result = result
	if result.Success {
		action.Status = model.StatusApproved
	} else {
		action.Status = model.StatusFailed
	}
	action.UpdatedAt = time.Now()

	if err := h.store.SaveAction(action); err != nil {
		h.logger.Error("failed to save approved action", "error", err)
		writeError(w, http.StatusInternalServerError, "failed to save action")
		return
	}

	h.logger.Info("action approved", "request_id", action.ID, "status", action.Status)
	writeJSON(w, http.StatusOK, action)
}

func (h *Handler) rejectAction(w http.ResponseWriter, r *http.Request) {
	id := r.PathValue("id")
	action, err := h.store.GetAction(id)
	if err != nil {
		writeError(w, http.StatusNotFound, "action not found")
		return
	}
	if action.Status != model.StatusPending {
		writeError(w, http.StatusConflict, "action is not pending")
		return
	}

	action.Status = model.StatusDenied
	action.Reason = "manually rejected"
	action.UpdatedAt = time.Now()

	if err := h.store.SaveAction(action); err != nil {
		h.logger.Error("failed to save rejected action", "error", err)
		writeError(w, http.StatusInternalServerError, "failed to save action")
		return
	}

	h.logger.Info("action rejected", "request_id", action.ID)
	writeJSON(w, http.StatusOK, action)
}

func (h *Handler) getPolicy(w http.ResponseWriter, r *http.Request) {
	p := h.engine.GetPolicy()
	writeJSON(w, http.StatusOK, p)
}

func (h *Handler) updatePolicy(w http.ResponseWriter, r *http.Request) {
	var req model.PolicyUpdateRequest
	if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
		writeError(w, http.StatusBadRequest, "invalid request body")
		return
	}

	p, err := h.engine.UpdatePolicy(req)
	if err != nil {
		h.logger.Error("failed to update policy", "error", err)
		writeError(w, http.StatusInternalServerError, "failed to update policy")
		return
	}

	h.logger.Info("policy updated",
		"auto_approve_limit", p.AutoApprovePaymentLimit,
		"denied_actions", p.DeniedActions,
	)
	writeJSON(w, http.StatusOK, p)
}

func (h *Handler) reset(w http.ResponseWriter, r *http.Request) {
	if err := h.store.DeleteAllActions(); err != nil {
		writeError(w, http.StatusInternalServerError, "failed to reset actions")
		return
	}
	if err := h.engine.ResetPolicy(); err != nil {
		writeError(w, http.StatusInternalServerError, "failed to reset policy")
		return
	}
	h.logger.Info("demo reset complete")
	writeJSON(w, http.StatusOK, map[string]string{"status": "reset complete"})
}

func (h *Handler) health(w http.ResponseWriter, r *http.Request) {
	writeJSON(w, http.StatusOK, map[string]string{"status": "ok"})
}

func writeJSON(w http.ResponseWriter, status int, v any) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	json.NewEncoder(w).Encode(v)
}

func writeError(w http.ResponseWriter, status int, msg string) {
	writeJSON(w, status, map[string]string{"error": msg})
}
