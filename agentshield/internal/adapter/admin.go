package adapter

import (
	"fmt"

	"github.com/agentshield/agentshield/internal/model"
)

// AdminAdapter simulates admin/infrastructure actions.
type AdminAdapter struct{}

func (a *AdminAdapter) Execute(req *model.ActionRequest) *model.ExecutionResult {
	switch req.ActionType {
	case "restart_service":
		svc, _ := req.Parameters["service"].(string)
		if svc == "" {
			svc = "unknown"
		}
		return &model.ExecutionResult{
			Success: true,
			Output:  fmt.Sprintf("service %s restarted successfully (mock)", svc),
		}
	case "delete_database":
		// This should never be reached if policy denies it,
		// but guard anyway.
		return &model.ExecutionResult{
			Success: false,
			Error:   "delete_database execution blocked at adapter level",
		}
	default:
		return &model.ExecutionResult{
			Success: true,
			Output:  fmt.Sprintf("admin action %s executed (mock)", req.ActionType),
		}
	}
}
