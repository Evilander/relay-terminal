import './tool-env.mjs';
import cli from '@tauri-apps/cli';

try {
  await cli.run(process.argv.slice(2), 'npm run tauri');
} catch (error) {
  console.error(error instanceof Error ? error.message : String(error));
  process.exitCode = 1;
}
