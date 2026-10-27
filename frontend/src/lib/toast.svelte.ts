export interface Toast {
  id: number;
  kind: 'success' | 'error';
  title: string;
  description?: string;
}

class Toasts {
  items = $state<Toast[]>([]);
  private next = 1;

  push(kind: Toast['kind'], title: string, description?: string): void {
    const id = this.next++;
    this.items.push({ id, kind, title, description });
    setTimeout(() => this.dismiss(id), 4000);
  }

  dismiss(id: number): void {
    this.items = this.items.filter((t) => t.id !== id);
  }
}

export const toasts = new Toasts();

export const notify = {
  ok: (title: string, description?: string) => toasts.push('success', title, description),
  error: (title: string, description?: string) => toasts.push('error', title, description),
};
