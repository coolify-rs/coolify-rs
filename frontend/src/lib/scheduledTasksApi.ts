// Page endpoints of the scheduled tasks section (crates/coolify-api/src/ui_shared/scheduled_tasks.rs).
import { request } from './api';
import { resPath } from './sharedApi';

export type TaskOwnerKind = 'application' | 'service';

export interface ScheduledTaskRow {
  id: number;
  uuid: string;
  name: string;
  command: string;
  frequency: string;
  container: string | null;
  timeout: number;
  /** Run the command with `sh -c` in the container. */
  use_shell: boolean;
  enabled: boolean;
  /** Why Coolify disabled the task automatically; cleared when it is enabled again. */
  disabled_reason: string | null;
  latest_status: string | null;
}

export interface ScheduledTasksIndex {
  can_update: boolean;
  type: string;
  server_timezone: string;
  is_running: boolean;
  container_names: string[];
  tasks: ScheduledTaskRow[];
}

export interface TaskExecution {
  id: number;
  status: string;
  created_at: string;
  finished_at: string | null;
  message: string | null;
}

export interface TaskForm {
  name: string;
  command: string;
  frequency: string;
  container: string;
  timeout: string | number;
  use_shell: boolean;
}

type Answer = { message: string };
const base = (kind: TaskOwnerKind, uuid: string) => `${resPath(kind, uuid)}/scheduled-tasks`;

export const scheduledTasksUi = {
  index: (kind: TaskOwnerKind, uuid: string) => request<ScheduledTasksIndex>('GET', base(kind, uuid)),
  add: (kind: TaskOwnerKind, uuid: string, body: TaskForm) => request<Answer>('POST', base(kind, uuid), body),
  save: (kind: TaskOwnerKind, uuid: string, task: string, body: TaskForm) => request<Answer>('POST', `${base(kind, uuid)}/${task}`, body),
  toggle: (kind: TaskOwnerKind, uuid: string, task: string) => request<Answer>('POST', `${base(kind, uuid)}/${task}/toggle`, {}),
  execute: (kind: TaskOwnerKind, uuid: string, task: string) => request<Answer>('POST', `${base(kind, uuid)}/${task}/execute`, {}),
  remove: (kind: TaskOwnerKind, uuid: string, task: string) => request<Answer>('DELETE', `${base(kind, uuid)}/${task}`),
  executions: (kind: TaskOwnerKind, uuid: string, task: string) =>
    request<{ can_read_output: boolean; executions: TaskExecution[] }>('GET', `${base(kind, uuid)}/${task}/executions`),
};
