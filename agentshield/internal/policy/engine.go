package policy

import (
	"encoding/json"
	"slices"
	"sync"
	"time"

	"github.com/agentshield/agentshield/internal/model"
	"github.com/agentshield/agentshield/internal/store"
)

// Engine evaluates actions against the active policy.
// It caches the policy in memory and reloads on update.
type Engine struct {
	mu     sync.RWMutex
	policy model.PolicyConfig
	store  *store.Store
}

// NewEngine creates a policy engine, loading the current policy from the store.
func NewEngine(s *store.Store) (*Engine, error) {
	p, err := s.GetPolicy()
	if err != nil {
		return nil, err
	}
	return &Engine{policy: *p, store: s}, nil
}

// Evaluate checks an action against the current policy and returns a decision with reason.
func (e *Engine) Evaluate(req *model.ActionRequest) (model.Decision, string) {
	e.mu.RLock()
	defer e.mu.RUnlock()

	// Check denied actions first.
	if slices.Contains(e.policy.DeniedActions, req.ActionType) {
		return model.DecisionDeny, "action is in denied list"
	}

	// Check approval-required actions.
	if slices.Contains(e.policy.ApprovalRequiredActions, req.ActionType) {
		return model.DecisionPending, "action requires manual approval"
	}

	// For payment actions, check amount against threshold.
	if req.ActionType == "send_payment" {
		if amount, ok := getAmount(req.Parameters); ok {
			if amount > e.policy.AutoApprovePaymentLimit {
				return model.DecisionPending, "payment amount exceeds auto-approve limit"
			}
			return model.DecisionAllow, "payment within auto-approve limit"
		}
		return model.DecisionPending, "payment amount not specified"
	}

	// Default: allow.
	return model.DecisionAllow, "action allowed by default policy"
}

// GetPolicy returns the current cached policy.
func (e *Engine) GetPolicy() model.PolicyConfig {
	e.mu.RLock()
	defer e.mu.RUnlock()
	return e.policy
}

// UpdatePolicy updates the policy in both cache and store.
func (e *Engine) UpdatePolicy(update model.PolicyUpdateRequest) (*model.PolicyConfig, error) {
	e.mu.Lock()
	defer e.mu.Unlock()

	if update.AutoApprovePaymentLimit != nil {
		e.policy.AutoApprovePaymentLimit = *update.AutoApprovePaymentLimit
	}
	if update.DeniedActions != nil {
		e.policy.DeniedActions = *update.DeniedActions
	}
	if update.ApprovalRequiredActions != nil {
		e.policy.ApprovalRequiredActions = *update.ApprovalRequiredActions
	}

	e.policy.UpdatedAt = time.Now()
	if err := e.store.SavePolicy(&e.policy); err != nil {
		return nil, err
	}

	p := e.policy
	return &p, nil
}

// ResetPolicy restores the default policy.
func (e *Engine) ResetPolicy() error {
	e.mu.Lock()
	defer e.mu.Unlock()

	e.policy = model.DefaultPolicy()
	return e.store.SavePolicy(&e.policy)
}

func getAmount(params map[string]any) (float64, bool) {
	v, ok := params["amount"]
	if !ok {
		return 0, false
	}
	switch a := v.(type) {
	case float64:
		return a, true
	case int:
		return float64(a), true
	case json.Number:
		f, err := a.Float64()
		return f, err == nil
	}
	return 0, false
}
