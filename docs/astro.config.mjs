// @ts-check
import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';

// GitHub Pages serves this repository's site under /dv, so the base has to
// match or every internal link 404s on the deployed site while working
// perfectly in dev.
export default defineConfig({
  site: 'https://adrum.github.io',
  base: '/dv',
  integrations: [
    starlight({
      title: 'dv',
      description:
        'A fast .NET SDK version manager: official SDKs, side-by-side runtimes and per-directory version resolution.',
      social: [
        { icon: 'github', label: 'GitHub', href: 'https://github.com/adrum/dv' },
      ],
      editLink: {
        baseUrl: 'https://github.com/adrum/dv/edit/main/docs/',
      },
      sidebar: [
        {
          label: 'Guides',
          items: [
            { label: 'Getting started', slug: 'guides/getting-started' },
            { label: 'How a version is chosen', slug: 'guides/resolution' },
            { label: 'Tools and workloads', slug: 'guides/tools-and-workloads' },
            { label: 'Troubleshooting', slug: 'guides/troubleshooting' },
          ],
        },
        {
          label: 'Reference',
          items: [
            { label: 'Commands', slug: 'reference/commands' },
            { label: 'global.json', slug: 'reference/global-json' },
            { label: 'How dv runs an SDK', slug: 'reference/install-roots' },
            { label: 'Platforms', slug: 'reference/platforms' },
            { label: 'Integrity', slug: 'reference/integrity' },
            { label: 'Scripting dv', slug: 'reference/scripting' },
          ],
        },
      ],
    }),
  ],
});
