// Endpoints of the layout (crates/coolify-api/src/ui_layout.rs).
import { request } from './api';
import type { NavContext } from './nav';

export interface QueueRow {
  id: number;
  application_name: string;
  deployment_url: string;
  deployment_uuid: string;
  pull_request_id: number;
  server_name: string | null;
  status: string;
  created_at: string;
  finished_at?: string | null;
  /** `ApplicationDeploymentQueue::isImageScanBlocked()`. */
  image_scan_blocked?: boolean;
  application_uuid: string;
  project: string | null;
  environment: string | null;
}

export interface SearchItem {
  name: string;
  type: string;
  subtype?: string;
  uuid?: string;
  description?: string | null;
  link?: string;
  project?: string | null;
  environment?: string | null;
  search_text: string;
}

export interface CreatableItem {
  name: string;
  description: string;
  quickcommand?: string;
  type: string;
  category: string;
  component?: string;
  route?: string;
  resourceType?: string;
  logo?: string;
  logo_default_url?: string;
  amd_only?: boolean;
  arm_only?: boolean;
}

export const layoutUi = {
  layout: () => request<{ is_sponsorship_popup_enabled: boolean; any_notification_enabled: boolean }>('GET', '/ui/layout'),
  deployments: () => request<{ deployments: QueueRow[] }>('GET', '/ui/layout/deployments'),
  recent: () => request<{ active: QueueRow[]; recent: QueueRow[] }>('GET', '/ui/layout/recent-deployments'),
  searchItems: () => request<{ resources: SearchItem[]; services: CreatableItem[] }>('GET', '/ui/search/items'),
  step: (step: string, id = '') => request<{ items: any[] }>('GET', `/ui/search/step?step=${step}&id=${encodeURIComponent(id)}`),
};

const nav = (name: string, description: string, link: string, search_text: string): SearchItem => ({ name, type: 'navigation', description, link, search_text });

/** `GlobalSearch::navigationItems`. */
export function navigationItems(c: NavContext, teamId: number | undefined): SearchItem[] {
  const list: (SearchItem | null)[] = [
    nav('Dashboard', 'Go to main dashboard', '/', 'dashboard home main overview'),
    nav('Servers', 'View all servers', '/servers', 'servers all list view'),
    nav('Projects', 'View all projects', '/projects', 'projects all list view'),
    nav('Destinations', 'View all destinations', '/destinations', 'destinations docker networks'),
    nav('Keys & Tokens', 'Manage private keys and API tokens', '/security/private-key', 'security private keys ssh api tokens cloud-init scripts'),
    c.isAdmin ? nav('Cloud-Init Scripts', 'Manage reusable cloud-init scripts', '/security/cloud-init-scripts', 'cloud-init scripts cloud init cloudinit initialization startup server setup') : null,
    nav('Sources', 'Manage GitHub apps and Git sources', '/sources', 'sources github apps git repositories'),
    nav('Storages', 'Manage S3 storage for backups', '/storages', 'storages s3 backups'),
    nav('Shared Variables', 'View all shared variables', '/shared-variables', 'shared variables environment all'),
    nav('Team Shared Variables', 'Manage team-wide shared variables', '/shared-variables/team', 'shared variables team environment'),
    nav('Project Shared Variables', 'Manage project shared variables', '/shared-variables/projects', 'shared variables project environment'),
    nav('Environment Shared Variables', 'Manage environment shared variables', '/shared-variables/environments', 'shared variables environment'),
    nav('Tags', 'View resources by tags', '/tags', 'tags labels organize'),
    c.isAdmin ? nav('Terminal', 'Access server terminal', '/terminal', 'terminal ssh console shell command line') : null,
    nav('Profile', 'Manage your profile and preferences', '/profile', 'profile account user settings preferences'),
    nav('Team', 'Manage team members and settings', '/team', 'team settings members users invitations'),
    nav('Notifications', 'Configure email, Discord, Telegram notifications', '/notifications/email', 'notifications alerts email discord telegram slack pushover'),
    nav('Analytics', 'View traffic analytics for your servers', '/analytics', 'analytics traffic requests visitors metrics'),
    c.isAdmin ? nav('Registries', 'Manage Docker registry credentials', '/registries', 'registries docker registry credentials login private images') : null,
    nav('Server Shared Variables', 'Manage server shared variables', '/shared-variables/servers', 'shared variables server environment'),
    nav('Team Members', 'Manage team members and invitations', '/team/members', 'team members users invitations invite roles'),
    c.isAdmin ? nav('Team Audit Log', 'View team activity history', '/team/audit-log', 'team audit log activity history events') : null,
    nav('Security', 'What each resource of your team needs, from its last image scan', '/vulnerabilities', 'vulnerabilities cve ghsa image scanning fixable packages security'),
    nav(
      'Team Image Scanning',
      'Scan rules and ignored vulnerabilities of the team',
      '/team/image-scanning',
      'team image scanning vulnerabilities cve scan rules alert block deploys ignored security',
    ),
    c.isInstanceAdmin ? nav('Team Admin View', 'Manage all users of this instance', '/team/admin', 'team admin view users instance delete') : null,
    c.isOwner ? nav('Team Danger Zone', 'Delete this team', '/team/danger', 'team danger zone delete remove') : null,
    nav('API Tokens', 'Manage API tokens', '/security/api-tokens', 'security api tokens personal access token'),
    c.isAdmin ? nav('Cloud Tokens', 'Manage cloud provider tokens', '/security/cloud-tokens', 'security cloud provider tokens hetzner digitalocean') : null,
    c.isAdmin ? nav('Integration Tokens', 'Manage integration tokens', '/security/integration-tokens', 'security integration tokens') : null,
    nav('Discord Notifications', 'Configure Discord notifications', '/notifications/discord', 'notifications alerts discord webhook'),
    nav('Telegram Notifications', 'Configure Telegram notifications', '/notifications/telegram', 'notifications alerts telegram bot'),
    nav('Slack Notifications', 'Configure Slack notifications', '/notifications/slack', 'notifications alerts slack webhook'),
    nav('Pushover Notifications', 'Configure Pushover notifications', '/notifications/pushover', 'notifications alerts pushover'),
    nav('Webhook Notifications', 'Configure webhook notifications', '/notifications/webhook', 'notifications alerts webhook http'),
    nav('Appearance', 'Change theme and display preferences', '/profile/appearance', 'profile appearance theme dark light mode preferences'),
  ];
  const out = list.filter((i): i is SearchItem => i !== null);
  // Instance settings only for the root team.
  if (teamId === 0) {
    out.push(
      nav('Settings', 'Instance settings and configuration', '/settings', 'settings general configuration instance url domain name timezone public ip https redirect'),
      nav('Advanced Settings', 'Registration, API access, MCP server, DNS validation and telemetry', '/settings/advanced', 'settings advanced registration api access allowed ips mcp server dns validation custom dns domain connect confirmation telemetry sponsorship cdn'),
      nav('Update Settings', 'Configure coolify-rs auto updates', '/settings/updates', 'settings updates auto update upgrade version frequency'),
      nav(
        'Image Scanning Settings',
        'Allow image scanning, the vulnerability database, and default scan rules',
        '/settings/image-scanning',
        'settings image scanning vulnerabilities cve scan rules grype database mirror alert block deploys security',
      ),
      nav('Instance Backup', 'Back up the coolify-rs instance database', '/settings/backup', 'settings instance backup coolify database restore'),
      nav('Instance Email', 'Configure SMTP or Resend for system emails', '/settings/email', 'settings instance email smtp resend transactional mail'),
      nav('Authentication', 'Configure OAuth login providers', '/settings/oauth', 'settings authentication oauth sso login providers github google gitlab azure'),
    );
  }
  return out;
}

const db = (name: string, description: string, type: string, logo: string): CreatableItem => ({
  name,
  description,
  quickcommand: `(type: new ${type})`,
  type,
  category: 'Databases',
  logo,
  resourceType: 'database',
});
const application = (name: string, description: string, quick: string, type: string): CreatableItem => ({
  name,
  description,
  quickcommand: `(type: new ${quick})`,
  type,
  category: 'Applications',
  resourceType: 'application',
});
const quick = (name: string, description: string, command: string, type: string, extra: Partial<CreatableItem>): CreatableItem => ({
  name,
  description,
  quickcommand: `(type: new ${command})`,
  type,
  category: 'Quick Actions',
  ...extra,
});

/** `GlobalSearch::loadCreatableItems`. */
export function creatableItems(isAdmin: boolean, services: CreatableItem[]): CreatableItem[] {
  const items: CreatableItem[] = [];
  if (isAdmin) items.push(quick('Project', 'Create a new project to organize your resources', 'project', 'project', { component: 'project.add-empty' }));
  if (isAdmin) items.push(quick('Server', 'Add a new server to deploy your applications', 'server', 'server', { component: 'server.create' }));
  items.push(quick('Team', 'Create a new team to collaborate with others', 'team', 'team', { component: 'team.create' }));
  if (isAdmin) {
    items.push(quick('Storage', 'Add S3, SFTP, FTPS, or WebDAV storage for backups', 'storage', 'storage', { component: 'storage.create' }));
    items.push(quick('Private Key', 'Add an SSH private key for server access', 'private key', 'private-key', { component: 'security.private-key.create' }));
    items.push(quick('GitHub App', 'Connect a GitHub app for source control', 'github', 'source', { component: 'source.github.create' }));
    items.push(quick('Canvas', 'Deploy resources visually on the deploy canvas', 'canvas', 'canvas', { route: 'canvas' }));
    items.push(
      application('Public Git Repository', 'Deploy from any public Git repository', 'public', 'public'),
      application('Private Repository (GitHub App)', 'Deploy private repositories through GitHub Apps', 'private github', 'private-gh-app'),
      application('Private Repository (Gitea App)', 'Deploy private repositories through Gitea Apps', 'private gitea', 'private-gitea-app'),
      application('Private Repository (Deploy Key)', 'Deploy private repositories with a deploy key', 'private deploy', 'private-deploy-key'),
      application('Dockerfile', 'Deploy a simple Dockerfile without Git', 'dockerfile', 'dockerfile'),
      application('Docker Compose', 'Deploy complex applications with Docker Compose', 'compose', 'docker-compose-empty'),
      application('Docker Image', 'Deploy an existing Docker image from any registry', 'image', 'docker-image'),
      db('PostgreSQL', 'Robust, advanced open-source database', 'postgresql', 'svgs/postgresql.svg'),
      db('MySQL', 'Popular open-source relational database', 'mysql', 'svgs/mysql.svg'),
      db('MariaDB', 'Community-developed fork of MySQL', 'mariadb', 'svgs/mariadb.svg'),
      { ...db('Redis / Valkey', 'In-memory data structure store (Redis or Valkey)', 'redis', 'svgs/resources/redis-valkey.svg'), quickcommand: '(type: new redis or new valkey)' },
      db('KeyDB', 'High-performance Redis alternative', 'keydb', 'svgs/keydb.svg'),
      db('Dragonfly', 'Modern in-memory datastore', 'dragonfly', 'svgs/dragonfly.svg'),
      db('MongoDB', 'Document-oriented NoSQL database', 'mongodb', 'svgs/mongodb.svg'),
      db('Clickhouse', 'Column-oriented database for analytics', 'clickhouse', 'svgs/clickhouse-icon.svg'),
      db('SQLite', 'Lightweight file-based relational database', 'sqlite', 'svgs/resources/sqlite.svg'),
    );
  }
  return items.concat(services);
}

/** `exactMatchCommands` of the Alpine component. */
export const exactMatchCommands = [
  'new project', 'new server', 'new team', 'new storage', 'new s3',
  'new private key', 'new privatekey', 'new key',
  'new github app', 'new github', 'new source', 'new canvas',
  'new public', 'new public git', 'new public repo', 'new public repository',
  'new private github', 'new private gh', 'new private deploy', 'new deploy key',
  'new dockerfile', 'new docker compose', 'new compose', 'new docker image', 'new image',
  'new postgresql', 'new postgres', 'new mysql', 'new mariadb',
  'new redis', 'new valkey', 'new keydb', 'new dragonfly', 'new mongodb', 'new mongo', 'new clickhouse', 'new sqlite',
];
