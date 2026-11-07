import { Info, type LucideIcon, Search } from 'lucide-react';
import type { ComponentProps, ReactNode, Ref } from 'react';
import { Empty, EmptyDescription, EmptyHeader, EmptyMedia, EmptyTitle } from '@/components/ui/empty';
import { Input } from '@/components/ui/input';
import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip';
import { cn } from '@/lib/utils';

// A page's title, with what the page is for behind an info mark, its tabs
// right after it, and what it does at the right end of the same row, so the
// commands keep their place whatever the title. They wrap under the title only
// when the window is too narrow for both. `wide` for a page whose list takes
// the whole width, so the row ends where the list does.
export function PageHeader({
  title,
  tabs,
  actions,
  wide = false,
  children,
}: {
  title: ReactNode;
  tabs?: ReactNode;
  actions?: ReactNode;
  wide?: boolean;
  children?: string;
}) {
  return (
    <header className={cn('flex flex-wrap items-center gap-x-4 gap-y-3', !wide && 'max-w-4xl')}>
      <div className="flex items-center gap-2">
        <h1 className="text-2xl font-semibold tracking-tight">{title}</h1>
        {children ? (
          <Tooltip>
            <TooltipTrigger asChild>
              <button
                type="button"
                aria-label={children}
                className="rounded-full text-muted-foreground outline-none hover:text-foreground focus-visible:ring-3 focus-visible:ring-ring/50"
              >
                <Info aria-hidden className="size-4" />
              </button>
            </TooltipTrigger>
            <TooltipContent>{children}</TooltipContent>
          </Tooltip>
        ) : null}
      </div>
      {tabs}
      {actions ? <div className="flex min-h-9 grow flex-wrap items-center justify-end gap-2">{actions}</div> : null}
    </header>
  );
}

// The search at the end of a page's row of actions.
export function SearchBox({
  value,
  label,
  placeholder = 'Search',
  onChange,
  ref,
}: {
  value: string;
  label: string;
  placeholder?: string;
  onChange: (value: string) => void;
  ref?: Ref<HTMLInputElement>;
}) {
  return (
    <div className="relative w-48">
      <Search
        aria-hidden
        className="pointer-events-none absolute top-1/2 left-3 size-4 -translate-y-1/2 text-muted-foreground"
      />
      <Input
        ref={ref}
        type="search"
        value={value}
        placeholder={placeholder}
        aria-label={label}
        className="pl-9"
        onChange={(e) => onChange(e.target.value)}
        onKeyDown={(e) => {
          if (e.key !== 'Escape') return;
          onChange('');
          e.currentTarget.blur();
        }}
      />
    </div>
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
