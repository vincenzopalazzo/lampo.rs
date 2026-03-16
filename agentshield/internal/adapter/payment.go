package adapter

import (
	"fmt"

	"github.com/agentshield/agentshield/internal/model"
)

// PaymentAdapter simulates payment execution (mock/Stripe test mode).
type PaymentAdapter struct{}

func (p *PaymentAdapter) Execute(req *model.ActionRequest) *model.ExecutionResult {
	amount, _ := req.Parameters["amount"].(float64)
	currency, _ := req.Parameters["currency"].(string)
	recipient, _ := req.Parameters["recipient"].(string)

	if currency == "" {
		currency = "USD"
	}
	if recipient == "" {
		recipient = "unknown"
	}

	return &model.ExecutionResult{
		Success: true,
		Output:  fmt.Sprintf("payment of %.2f %s sent to %s (mock)", amount, currency, recipient),
	}
}
