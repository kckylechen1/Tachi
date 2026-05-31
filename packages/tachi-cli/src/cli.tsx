import React from 'react';
import { render } from 'ink';
import { Command } from 'commander';
import { MainMenu } from './components/MainMenu.js';
import { initConfig } from './utils/config.js';
import { TACHI_CLI_VERSION } from './version.js';

const program = new Command();

program
  .name('tachi')
  .description('Tachi CLI - Memory and Capability Orchestrator')
  .version(TACHI_CLI_VERSION);

program
  .command('menu', { isDefault: true })
  .description('Open interactive main menu')
  .action(async () => {
    await initConfig();
    render(React.createElement(MainMenu));
  });

program
  .command('init')
  .description('Run first-time setup wizard')
  .action(async () => {
    const { InitWizard } = await import('./components/InitWizard.js');
    await initConfig();
    render(React.createElement(InitWizard));
  });

program
  .command('start')
  .description('Start Tachi daemon')
  .action(async () => {
    const { StartCommand } = await import('./commands/start.js');
    await initConfig();
    render(React.createElement(StartCommand));
  });

program
  .command('stop')
  .description('Stop Tachi daemon')
  .action(async () => {
    const { StopCommand } = await import('./commands/stop.js');
    await initConfig();
    render(React.createElement(StopCommand));
  });

program
  .command('status')
  .description('Check daemon status')
  .action(async () => {
    const { StatusCommand } = await import('./commands/status.js');
    await initConfig();
    render(React.createElement(StatusCommand));
  });

program.parse();