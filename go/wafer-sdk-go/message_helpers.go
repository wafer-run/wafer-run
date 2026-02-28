package wafer

import "strings"

// Action returns the semantic request action (e.g. "retrieve", "create", "update", "delete").
func (m *Message) Action() string { return m.GetMeta("req.action") }

// Path returns the request resource path.
func (m *Message) Path() string { return m.GetMeta("req.resource") }

// Query returns a query parameter value by key.
func (m *Message) Query(key string) string { return m.GetMeta("req.query." + key) }

// UserID returns the authenticated user's ID.
func (m *Message) UserID() string { return m.GetMeta("auth.user_id") }

// UserEmail returns the authenticated user's email.
func (m *Message) UserEmail() string { return m.GetMeta("auth.user_email") }

// UserRoles returns the authenticated user's roles as a slice.
func (m *Message) UserRoles() []string {
	roles := m.GetMeta("auth.user_roles")
	if roles == "" {
		return nil
	}
	return strings.Split(roles, ",")
}

// IsAdmin returns true if the user has the "admin" role.
func (m *Message) IsAdmin() bool {
	for _, r := range m.UserRoles() {
		if r == "admin" {
			return true
		}
	}
	return false
}

// Var returns a path variable extracted by the router.
func (m *Message) Var(name string) string { return m.GetMeta("req.param." + name) }

// Header returns a request header value (case-insensitive key).
func (m *Message) Header(name string) string {
	return m.GetMeta("http.header." + strings.ToLower(name))
}

// Cookie returns a named cookie value from the Cookie header.
func (m *Message) Cookie(name string) string {
	raw := m.GetMeta("http.header.Cookie")
	if raw == "" {
		// Try lowercase
		raw = m.GetMeta("http.header.cookie")
	}
	if raw == "" {
		return ""
	}
	for _, part := range strings.Split(raw, ";") {
		part = strings.TrimSpace(part)
		eq := strings.IndexByte(part, '=')
		if eq < 0 {
			continue
		}
		if part[:eq] == name {
			return part[eq+1:]
		}
	}
	return ""
}

// ContentType returns the request content type.
func (m *Message) ContentType() string { return m.GetMeta("req.content_type") }

// RemoteAddr returns the client's remote address.
func (m *Message) RemoteAddr() string { return m.GetMeta("req.client_ip") }
