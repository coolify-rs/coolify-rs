// What the page does after `configureDnsAfterDomainAdd`. Upstream's `ConfigureDnsRecordJob` ends with
// the broadcast `DnsRecordConfigurationFinished`; here the page asks until its jobs ended.
import { request } from './api';
import { resPath, type ResourceKind } from './sharedApi';
import { notify } from './toast.svelte';

export interface AfterAddDns {
  pending?: string[];
  pending_message?: string;
  open_modal?: boolean;
  jobs?: string[];
  info?: string[];
}

interface Finished {
  job: string;
  successful: boolean;
  message: string;
  hostname: string;
}

const POLL_MS = 2000;
const POLL_ROUNDS = 60;

/** `dnsRecordConfigurationFinished`: the toast of each job, and a reload for its row. */
export function followDnsJobs(kind: ResourceKind, uuid: string, jobs: string[], reload: () => unknown): void {
  let open = [...jobs];
  let round = 0;
  const tick = async () => {
    round += 1;
    try {
      const r = await request<{ finished: Finished[] }>('GET', `${resPath(kind, uuid)}/dns/finished?jobs=${open.join(',')}`);
      for (const f of r.finished) {
        open = open.filter((j) => j !== f.job);
        if (f.successful) notify.success(f.message);
        else notify.failure(`DNS record could not be added for ${f.hostname}: ${f.message}`);
      }
      if (r.finished.length > 0) await reload();
    } catch {
      // The next round asks again.
    }
    if (open.length > 0 && round < POLL_ROUNDS) setTimeout(tick, POLL_MS);
  };
  setTimeout(tick, POLL_MS);
}

export function hostOf(url: string): string {
  try {
    return new URL(url).hostname.toLowerCase();
  } catch {
    return url;
  }
}
