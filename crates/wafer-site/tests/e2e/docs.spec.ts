import { test, expect } from '@playwright/test';

// ─── DOCS SUB-PAGES ──────────────────────────────────────────

const docPages = [
  { path: '/docs', title: 'Quick Start', heading: 'Quick Start' },
  { path: '/docs/creating-a-block', title: 'Creating a Block', heading: 'Creating a Block' },
  { path: '/docs/running-a-block', title: 'Running a Block', heading: 'Running a Block' },
  { path: '/docs/cli', title: 'CLI', heading: 'CLI' },
  { path: '/docs/chain-configuration', title: 'Chain Configuration', heading: 'Chain Configuration' },
  { path: '/docs/built-in-blocks', title: 'Built-in Blocks', heading: 'Built-in Blocks' },
  { path: '/docs/services', title: 'Services', heading: 'Services' },
  { path: '/docs/http-bridge', title: 'HTTP Bridge', heading: 'HTTP Bridge' },
  { path: '/docs/api-reference', title: 'API Reference', heading: 'API Reference' },
  { path: '/docs/registry', title: 'Block Registry', heading: 'Block Registry' },
  { path: '/docs/deployment', title: 'Deployment', heading: 'Deployment' },
];

test.describe('Docs Sub-Pages', () => {
  for (const doc of docPages) {
    test(`${doc.path} loads with correct content`, async ({ page }) => {
      const resp = await page.goto(doc.path);
      expect(resp?.status()).toBe(200);

      // Page has the Documentation banner
      await expect(page.locator('.page-title h1')).toHaveText('Documentation');

      // Content heading matches
      await expect(page.locator('.docs-content h2').first()).toHaveText(doc.heading);

      // Sidebar is present with 11 links
      const sidebarLinks = page.locator('.sidebar a');
      await expect(sidebarLinks).toHaveCount(11);

      // Current page link has active class
      const activeLink = page.locator('.sidebar a.active');
      await expect(activeLink).toHaveCount(1);
      await expect(activeLink).toHaveAttribute('href', doc.path);
    });
  }
});

// ─── SIDEBAR NAVIGATION ─────────────────────────────────────

test.describe('Docs Sidebar Navigation', () => {
  test('clicking sidebar links navigates between doc pages', async ({ page }) => {
    await page.goto('/docs');

    // Click Creating a Block
    await page.click('.sidebar a[href="/docs/creating-a-block"]');
    await expect(page).toHaveURL('/docs/creating-a-block');
    await expect(page.locator('.docs-content h2').first()).toHaveText('Creating a Block');
    await expect(page.locator('.sidebar a.active')).toHaveAttribute('href', '/docs/creating-a-block');

    // Click Deployment
    await page.click('.sidebar a[href="/docs/deployment"]');
    await expect(page).toHaveURL('/docs/deployment');
    await expect(page.locator('.docs-content h2').first()).toHaveText('Deployment');
    await expect(page.locator('.sidebar a.active')).toHaveAttribute('href', '/docs/deployment');

    // Click back to Quick Start
    await page.click('.sidebar a[href="/docs"]');
    await expect(page).toHaveURL('/docs');
    await expect(page.locator('.docs-content h2').first()).toHaveText('Quick Start');
  });
});
