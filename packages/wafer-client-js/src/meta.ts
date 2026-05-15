// Meta key constants. The canonical definitions live in
// common/definitions/meta_keys.toml; this file re-exports the codegen
// output so the SDK stays in sync with the Rust + Go runtime constants.
//
// To regenerate after editing the TOML: python3 common/codegen/generate.py

export {
  META_REQ_ACTION,
  META_REQ_RESOURCE,
  META_REQ_PARAM_PREFIX,
  META_REQ_QUERY_PREFIX,
  META_REQ_CLIENT_IP,
  META_REQ_CONTENT_TYPE,
  META_AUTH_USER_ID,
  META_AUTH_USER_EMAIL,
  META_AUTH_USER_ROLES,
  META_RESP_STATUS,
  META_RESP_CONTENT_TYPE,
  META_RESP_HEADER_PREFIX,
  META_RESP_COOKIE_PREFIX,
} from './generated/meta-keys';
