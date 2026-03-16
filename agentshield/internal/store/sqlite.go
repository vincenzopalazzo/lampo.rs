package store

import (
	"database/sql"
	"encoding/json"
	"fmt"
	"time"

	"github.com/agentshield/agentshield/internal/model"
	_ "github.com/mattn/go-sqlite3"
)

// Store provides persistence for actions and policies.
type Store struct {
	db *sql.DB
}

// New creates a new Store with the given SQLite database path.
func New(dbPath string) (*Store, error) {
	db, err := sql.Open("sqlite3", dbPath+"?_journal_mode=WAL")
	if err != nil {
		return nil, fmt.Errorf("open db: %w", err)
	}
	s := &Store{db: db}
	if err := s.migrate(); err != nil {
		return nil, fmt.Errorf("migrate: %w", err)
	}
	return s, nil
}

func (s *Store) migrate() error {
	_, err := s.db.Exec(`
		CREATE TABLE IF NOT EXISTS actions (
			id TEXT PRIMARY KEY,
			agent_id TEXT NOT NULL,
			action_type TEXT NOT NULL,
			parameters TEXT NOT NULL,
			status TEXT NOT NULL,
			decision TEXT NOT NULL,
			reason TEXT NOT NULL DEFAULT '',
			result TEXT,
			created_at DATETIME NOT NULL,
			updated_at DATETIME NOT NULL
		);
		CREATE TABLE IF NOT EXISTS policy (
			id INTEGER PRIMARY KEY CHECK (id = 1),
			auto_approve_payment_limit REAL NOT NULL,
			denied_actions TEXT NOT NULL,
			approval_required_actions TEXT NOT NULL,
			updated_at DATETIME NOT NULL
		);
		CREATE INDEX IF NOT EXISTS idx_actions_status ON actions(status);
		CREATE INDEX IF NOT EXISTS idx_actions_agent ON actions(agent_id);
	`)
	return err
}

// SaveAction persists an action request.
func (s *Store) SaveAction(a *model.ActionRequest) error {
	params, _ := json.Marshal(a.Parameters)
	var result []byte
	if a.Result != nil {
		result, _ = json.Marshal(a.Result)
	}
	_, err := s.db.Exec(`
		INSERT INTO actions (id, agent_id, action_type, parameters, status, decision, reason, result, created_at, updated_at)
		VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
		ON CONFLICT(id) DO UPDATE SET
			status=excluded.status,
			decision=excluded.decision,
			reason=excluded.reason,
			result=excluded.result,
			updated_at=excluded.updated_at
	`, a.ID, a.AgentID, a.ActionType, string(params), a.Status, a.Decision, a.Reason, string(result), a.CreatedAt, a.UpdatedAt)
	return err
}

// GetAction retrieves an action by ID.
func (s *Store) GetAction(id string) (*model.ActionRequest, error) {
	row := s.db.QueryRow(`SELECT id, agent_id, action_type, parameters, status, decision, reason, result, created_at, updated_at FROM actions WHERE id = ?`, id)
	return scanAction(row)
}

// ListActions returns actions filtered by status. If status is empty, returns all.
func (s *Store) ListActions(status string) ([]*model.ActionRequest, error) {
	var rows *sql.Rows
	var err error
	if status != "" {
		rows, err = s.db.Query(`SELECT id, agent_id, action_type, parameters, status, decision, reason, result, created_at, updated_at FROM actions WHERE status = ? ORDER BY created_at DESC`, status)
	} else {
		rows, err = s.db.Query(`SELECT id, agent_id, action_type, parameters, status, decision, reason, result, created_at, updated_at FROM actions ORDER BY created_at DESC`)
	}
	if err != nil {
		return nil, err
	}
	defer rows.Close()

	var actions []*model.ActionRequest
	for rows.Next() {
		a, err := scanActionRows(rows)
		if err != nil {
			return nil, err
		}
		actions = append(actions, a)
	}
	return actions, rows.Err()
}

type scanner interface {
	Scan(dest ...any) error
}

func scanAction(row *sql.Row) (*model.ActionRequest, error) {
	a := &model.ActionRequest{}
	var params, result string
	err := row.Scan(&a.ID, &a.AgentID, &a.ActionType, &params, &a.Status, &a.Decision, &a.Reason, &result, &a.CreatedAt, &a.UpdatedAt)
	if err != nil {
		return nil, err
	}
	json.Unmarshal([]byte(params), &a.Parameters)
	if result != "" {
		a.Result = &model.ExecutionResult{}
		json.Unmarshal([]byte(result), a.Result)
	}
	return a, nil
}

func scanActionRows(rows *sql.Rows) (*model.ActionRequest, error) {
	a := &model.ActionRequest{}
	var params, result string
	err := rows.Scan(&a.ID, &a.AgentID, &a.ActionType, &params, &a.Status, &a.Decision, &a.Reason, &result, &a.CreatedAt, &a.UpdatedAt)
	if err != nil {
		return nil, err
	}
	json.Unmarshal([]byte(params), &a.Parameters)
	if result != "" {
		a.Result = &model.ExecutionResult{}
		json.Unmarshal([]byte(result), a.Result)
	}
	return a, nil
}

// GetPolicy returns the current policy, creating default if none exists.
func (s *Store) GetPolicy() (*model.PolicyConfig, error) {
	row := s.db.QueryRow(`SELECT auto_approve_payment_limit, denied_actions, approval_required_actions, updated_at FROM policy WHERE id = 1`)
	p := &model.PolicyConfig{}
	var denied, approval string
	err := row.Scan(&p.AutoApprovePaymentLimit, &denied, &approval, &p.UpdatedAt)
	if err == sql.ErrNoRows {
		def := model.DefaultPolicy()
		if err := s.SavePolicy(&def); err != nil {
			return nil, err
		}
		return &def, nil
	}
	if err != nil {
		return nil, err
	}
	json.Unmarshal([]byte(denied), &p.DeniedActions)
	json.Unmarshal([]byte(approval), &p.ApprovalRequiredActions)
	return p, nil
}

// SavePolicy persists the policy configuration.
func (s *Store) SavePolicy(p *model.PolicyConfig) error {
	denied, _ := json.Marshal(p.DeniedActions)
	approval, _ := json.Marshal(p.ApprovalRequiredActions)
	_, err := s.db.Exec(`
		INSERT INTO policy (id, auto_approve_payment_limit, denied_actions, approval_required_actions, updated_at)
		VALUES (1, ?, ?, ?, ?)
		ON CONFLICT(id) DO UPDATE SET
			auto_approve_payment_limit=excluded.auto_approve_payment_limit,
			denied_actions=excluded.denied_actions,
			approval_required_actions=excluded.approval_required_actions,
			updated_at=excluded.updated_at
	`, p.AutoApprovePaymentLimit, string(denied), string(approval), p.UpdatedAt)
	return err
}

// DeleteAllActions removes all actions (used for demo reset).
func (s *Store) DeleteAllActions() error {
	_, err := s.db.Exec(`DELETE FROM actions`)
	return err
}

// ResetPolicy restores the default policy.
func (s *Store) ResetPolicy() error {
	def := model.DefaultPolicy()
	def.UpdatedAt = time.Now()
	return s.SavePolicy(&def)
}

// Close closes the database connection.
func (s *Store) Close() error {
	return s.db.Close()
}
