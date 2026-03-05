import type {SidebarsConfig} from '@docusaurus/plugin-content-docs';

const sidebars: SidebarsConfig = {
  specSidebar: [
    {
      type: 'doc',
      id: 'WAFER_SPEC',
      label: 'Core Specification',
    },
    {
      type: 'doc',
      id: 'WAFER_GO',
      label: 'Go Implementation',
    },
  ],
};

export default sidebars;
