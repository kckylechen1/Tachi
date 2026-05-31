import chalk from 'chalk';
import { TACHI_CLI_VERSION } from '../version.js';

export const banner = `
${chalk.cyan('╔═══════════════════════════════════════════════════════════════╗')}
${chalk.cyan('║')}                                                               ${chalk.cyan('║')}
${chalk.cyan('║')}    ${chalk.bold.white('████████╗ █████╗  ██████╗██╗  ██╗██╗')}                     ${chalk.cyan('║')}
${chalk.cyan('║')}    ${chalk.bold.white('╚══██╔══╝██╔══██╗██╔════╝██║  ██║██║')}                     ${chalk.cyan('║')}
${chalk.cyan('║')}       ${chalk.bold.white('██║   ███████║██║     ███████║██║')}                     ${chalk.cyan('║')}
${chalk.cyan('║')}       ${chalk.bold.white('██║   ██╔══██║██║     ██╔══██║██║')}                     ${chalk.cyan('║')}
${chalk.cyan('║')}       ${chalk.bold.white('██║   ██║  ██║╚██████╗██║  ██║██║')}                     ${chalk.cyan('║')}
${chalk.cyan('║')}       ${chalk.bold.white('╚═╝   ╚═╝  ╚═╝ ╚═════╝╚═╝  ╚═╝╚═╝')}                     ${chalk.cyan('║')}
${chalk.cyan('║')}                                                               ${chalk.cyan('║')}
${chalk.cyan('║')}              ${chalk.gray('Memory \u0026 Capability Orchestrator')}                 ${chalk.cyan('║')}
${chalk.cyan('║')}                       ${chalk.gray(`v${TACHI_CLI_VERSION}`)}                                 ${chalk.cyan('║')}
${chalk.cyan('╚═══════════════════════════════════════════════════════════════╝')}
`;

export function drawBox(title: string, content: string, width = 65): string {
  const lines = content.split('\n');
  const top = `${chalk.cyan('╔')}${'═'.repeat(width - 2)}${chalk.cyan('╗')}`;
  const bottom = `${chalk.cyan('╚')}${'═'.repeat(width - 2)}${chalk.cyan('╝')}`;
  
  const titleLine = title 
    ? `${chalk.cyan('║')}${chalk.bold.white(title.padStart((width + title.length) / 2).padEnd(width - 2))}${chalk.cyan('║')}`
    : `${chalk.cyan('║')}${' '.repeat(width - 2)}${chalk.cyan('║')}`;
  
  const contentLines = lines.map(line => 
    `${chalk.cyan('║')} ${line.padEnd(width - 3)}${chalk.cyan('║')}`
  );
  
  return [top, titleLine, ...contentLines, bottom].join('\n');
}

export function drawMenu(title: string, items: string[], selectedIndex: number, width = 65): string {
  const top = `${chalk.cyan('╔')}${'═'.repeat(width - 2)}${chalk.cyan('╗')}`;
  const bottom = `${chalk.cyan('╚')}${'═'.repeat(width - 2)}${chalk.cyan('╝')}`;
  
  const titleLine = `${chalk.cyan('║')}${chalk.bold.white(title.padStart((width + title.length) / 2).padEnd(width - 2))}${chalk.cyan('║')}`;
  const separator = `${chalk.cyan('╠')}${'═'.repeat(width - 2)}${chalk.cyan('╣')}`;
  
  const itemLines = items.map((item, index) => {
    const isSelected = index === selectedIndex;
    const prefix = isSelected ? chalk.cyan('❯ ') : '  ';
    const styledItem = isSelected ? chalk.cyan(item) : item;
    const line = `${prefix}${styledItem}`;
    return `${chalk.cyan('║')} ${line.padEnd(width - 3)}${chalk.cyan('║')}`;
  });
  
  return [top, titleLine, separator, ...itemLines, bottom].join('\n');
}

export const colors = {
  primary: chalk.cyan,
  success: chalk.green,
  error: chalk.red,
  warning: chalk.yellow,
  info: chalk.blue,
  text: chalk.white,
  dim: chalk.gray,
  bold: chalk.bold,
};