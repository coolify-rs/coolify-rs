// Toast queue (port of upstream components/toast.blade.php behaviour:
// 4s lifetime, paused while hovered).

export type ToastType = 'success' | 'info' | 'warning' | 'danger' | 'default';

export interface Toast {
  id: number;
  type: ToastType;
  message: string;
  description?: string;
  copied?: boolean;
}

class Toasts {
  items = $state<Toast[]>([]);
  private next = 1;
  private timers = new Map<number, ReturnType<typeof setTimeout>>();

  push(type: ToastType, message: string, description?: string): void {
    const id = this.next++;
    // `this.toasts.unshift(toast)` under `flex-col-reverse`: the newest is at the bottom.
    this.items.unshift({ id, type, message, description });
    this.schedule(id, 4000);
  }

  schedule(id: number, delay: number): void {
    clearTimeout(this.timers.get(id));
    this.timers.set(id, setTimeout(() => this.dismiss(id), delay));
  }

  pause(id: number): void {
    clearTimeout(this.timers.get(id));
  }

  dismiss(id: number): void {
    clearTimeout(this.timers.get(id));
    this.timers.delete(id);
    this.items = this.items.filter((t) => t.id !== id);
  }
}

export const toasts = new Toasts();

export const notify = {
  ok: (message: string, description?: string) => toasts.push('success', message, description),
  error: (message: string, description?: string) => toasts.push('danger', message, description),
  info: (message: string, description?: string) => toasts.push('info', message, description),
  /** Livewire `$this->dispatch('success', $message)`: titled "Success" (layouts/base.blade.php). */
  success: (message: string) => toasts.push('success', 'Success', message),
  /** Livewire `$this->dispatch('error', $message)` (`handleError()`): titled "Error". */
  failure: (message: string) => toasts.push('danger', 'Error', message),
};
