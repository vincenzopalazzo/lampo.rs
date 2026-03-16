package adapter

import (
	"fmt"

	"github.com/agentshield/agentshield/internal/model"
)

// Adapter executes an action and returns the result.
type Adapter interface {
	Execute(req *model.ActionRequest) *model.ExecutionResult
}

// Registry maps action types to their execution adapters.
type Registry struct {
	adapters map[string]Adapter
}

// NewRegistry creates a registry with the default adapters.
func NewRegistry() *Registry {
	r := &Registry{adapters: make(map[string]Adapter)}
	r.Register("send_payment", &PaymentAdapter{})
	r.Register("restart_service", &AdminAdapter{})
	r.Register("delete_database", &AdminAdapter{})
	return r
}

// Register adds an adapter for the given action type.
func (r *Registry) Register(actionType string, a Adapter) {
	r.adapters[actionType] = a
}

// Execute finds and runs the adapter for the given action.
func (r *Registry) Execute(req *model.ActionRequest) *model.ExecutionResult {
	a, ok := r.adapters[req.ActionType]
	if !ok {
		return &model.ExecutionResult{
			Success: false,
			Error:   fmt.Sprintf("no adapter registered for action type: %s", req.ActionType),
		}
	}
	return a.Execute(req)
}
