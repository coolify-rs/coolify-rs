// `EnvironmentVariableAnalyzer::getProblematicBuildVariables()` (the frontend part), used by
// components/environment-variable-warning.blade.php.
interface Problem {
  values: string[];
  issue: string;
  recommendation: string;
}

const PROBLEMS: Record<string, Problem> = {
  NODE_ENV: {
    values: ['production', 'prod'],
    issue: 'Skips devDependencies installation which are often required for building (webpack, typescript, etc.)',
    recommendation: 'Uncheck "Available at Buildtime" or use "development" during build',
  },
  NPM_CONFIG_PRODUCTION: {
    values: ['true', '1', 'yes'],
    issue: 'Forces npm to skip devDependencies',
    recommendation: 'Remove from build-time variables or set to false',
  },
  YARN_PRODUCTION: {
    values: ['true', '1', 'yes'],
    issue: 'Forces yarn to skip devDependencies',
    recommendation: 'Remove from build-time variables or set to false',
  },
  COMPOSER_NO_DEV: {
    values: ['1', 'true', 'yes'],
    issue: 'Skips require-dev packages which may include build tools',
    recommendation: 'Set as "Runtime only" or remove from build-time variables',
  },
  MIX_ENV: {
    values: ['prod', 'production'],
    issue: 'Production mode may skip development dependencies needed for compilation',
    recommendation: 'Use "dev" for build or set as "Runtime only"',
  },
  RAILS_ENV: {
    values: ['production'],
    issue: 'May affect asset precompilation and dependency handling',
    recommendation: 'Consider using "development" for build phase',
  },
  RACK_ENV: {
    values: ['production'],
    issue: 'May affect dependency handling and build behavior',
    recommendation: 'Consider using "development" for build phase',
  },
  BUNDLE_WITHOUT: {
    values: ['development', 'test', 'development:test'],
    issue: 'Excludes gem groups that may contain build dependencies',
    recommendation: 'Remove from build-time variables or adjust groups',
  },
  FLASK_ENV: {
    values: ['production'],
    issue: 'May affect debug mode and development tools availability',
    recommendation: 'Usually safe, but consider "development" for complex builds',
  },
  APP_ENV: {
    values: ['production', 'prod'],
    issue: 'May affect dependency installation and build optimizations',
    recommendation: 'Consider using "local" or "development" for build',
  },
};

/** The warning for a buildtime variable, or null. */
export function buildWarning(key: string, value: string | null, isBuildtime: boolean): Problem | null {
  if (!isBuildtime || !key) return null;
  const config = PROBLEMS[key];
  if (!config) return null;
  const lower = String(value ?? '').toLowerCase();
  return config.values.some((v) => v.toLowerCase() === lower) ? config : null;
}
