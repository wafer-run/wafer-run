import { test, expect } from '@playwright/test';

// The inspector requires authentication (auth.user_id meta).
// wafer-site doesn't include an auth pipeline for inspector routes,
// so all inspector endpoints return 401.

test.describe('Inspector Auth', () => {
  test('/_inspector/ui returns 401 without auth', async ({ request }) => {
    const resp = await request.get('/_inspector/ui');
    expect(resp.status()).toBe(401);
  });

  test('/_inspector/blocks returns 401 without auth', async ({ request }) => {
    const resp = await request.get('/_inspector/blocks');
    expect(resp.status()).toBe(401);
  });

  test('/_inspector/flows returns 401 without auth', async ({ request }) => {
    const resp = await request.get('/_inspector/flows');
    expect(resp.status()).toBe(401);
  });
});
