package model

import "time"

// Decision represents the policy engine's decision on an action.
type Decision string

const (
	DecisionAllow   Decision = "allow"
	DecisionDeny    Decision = "deny"
	DecisionPending Decision = "pending"
)

// ActionStatus represents the current state of an action request.
type ActionStatus string

const (
	StatusPending  ActionStatus = "pending"
	StatusApproved ActionStatus = "approved"
	StatusDenied   ActionStatus = "denied"
	StatusExecuted ActionStatus = "executed"
	StatusFailed   ActionStatus = "failed"
)

// ActionRequest is an action proposed by an AI agent.
type ActionRequest struct {
	ID          string            `json:"id"`
	AgentID     string            `json:"agent_id"`
	ActionType  string            `json:"action_type"`
	Parameters  map[string]any    `json:"parameters"`
	Status      ActionStatus      `json:"status"`
	Decision    Decision          `json:"decision"`
	Reason      string            `json:"reason"`
	Result      *ExecutionResult  `json:"result,omitempty"`
	CreatedAt   time.Time         `json:"created_at"`
	UpdatedAt   time.Time         `json:"updated_at"`
}

// ExecutionResult holds the outcome of executing an action.
type ExecutionResult struct {
	Success bool   `json:"success"`
	Output  string `json:"output"`
	Error   string `json:"error,omitempty"`
}

// PolicyConfig holds the active policy configuration.
type PolicyConfig struct {
	AutoApprovePaymentLimit float64  `json:"auto_approve_payment_limit"`
	DeniedActions           []string `json:"denied_actions"`
	ApprovalRequiredActions  []string `json:"approval_required_actions"`
	UpdatedAt               time.Time `json:"updated_at"`
}

// DefaultPolicy returns the default policy configuration.
func DefaultPolicy() PolicyConfig {
	return PolicyConfig{
		AutoApprovePaymentLimit: 100,
		DeniedActions:           []string{"delete_database"},
		ApprovalRequiredActions:  []string{},
		UpdatedAt:               time.Now(),
	}
}

// SubmitActionRequest is the API request body for submitting an action.
type SubmitActionRequest struct {
	AgentID    string         `json:"agent_id"`
	ActionType string         `json:"action_type"`
	Parameters map[string]any `json:"parameters"`
}

// PolicyUpdateRequest is the API request body for updating policy.
type PolicyUpdateRequest struct {
	AutoApprovePaymentLimit *float64  `json:"auto_approve_payment_limit,omitempty"`
	DeniedActions           *[]string `json:"denied_actions,omitempty"`
	ApprovalRequiredActions  *[]string `json:"approval_required_actions,omitempty"`
}
