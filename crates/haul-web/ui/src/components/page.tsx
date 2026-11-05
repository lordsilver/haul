import type { LucideIcon } from 'lucide-react';
import type { ComponentProps, ReactNode } from 'react';
import { Empty, EmptyDescription, EmptyHeader, EmptyMedia, EmptyTitle } from '@/components/ui/empty';
import { cn } from '@/lib/utils';

// A page's title, what it does on its right, and what it is for below.
export function PageHeader({
  title,
  actions,
  children,
}: {
  title: ReactNode;
  actions?: ReactNode;
  children?: ReactNode;
}) {
  return (
    <header className="flex flex-wrap items-center gap-x-4 gap-y-3">
      {/* Words need room to sit in one line; a title alone leaves it to the actions. */}
      <div className={cn('min-w-0', children ? 'flex-[1_1_320px]' : 'mr-auto')}>
        <h1 className="text-2xl font-semibold tracking-tight">{title}</h1>
        {children ? <p className="mt-1 max-w-3xl text-sm text-muted-foreground">{children}</p> : null}
      </div>
      {actions ? <div className="flex flex-wrap items-center gap-2">{actions}</div> : null}
    </header>
  );
}

// The part of a page that scrolls under its header: a column of cards.
export function PageScroll({ children, ...props }: Omit<ComponentProps<'div'>, 'className'>) {
  return (
    <div className="-mx-2 min-h-0 flex-1 overflow-x-hidden overflow-y-auto px-2 pb-4" {...props}>
      <div className="flex max-w-4xl flex-col gap-2.5">{children}</div>
    </div>
  );
}

// What a page says when it has nothing to show; `children` go below the words.
export function EmptyState({
  icon: Icon,
  title,
  description,
  bordered = true,
  children,
}: {
  icon: LucideIcon;
  title: ReactNode;
  description: ReactNode;
  bordered?: boolean;
  children?: ReactNode;
}) {
  return (
    <Empty className={cn('flex-1', bordered && 'border')}>
      <EmptyHeader>
        <EmptyMedia variant="icon">
          <Icon />
        </EmptyMedia>
        <EmptyTitle>{title}</EmptyTitle>
        <EmptyDescription>{description}</EmptyDescription>
      </EmptyHeader>
      {children}
    </Empty>
  );
}
