export { WaferClient } from './client';
export type { RequestOptions } from './client';

export type { WaferConfig } from './types/config';
export type { WaferMessage, WaferMeta } from './types/message';
export type { WaferResult, WaferResponse } from './types/result';
export { WaferError } from './types/error';
export type { WaferErrorCode } from './types/error';

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
} from './meta';
