import type { ComponentProps, ReactNode } from 'react';
import { Button } from '@/components/ui/button';
import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip';
import { cn } from '@/lib/utils';

// A button that is only an icon: its label is its name for screen readers
// and its tooltip.
export function IconButton({
  label,
  children,
  size = 'icon-sm',
  variant = 'ghost',
  className,
  ...props
}: { label: string; children: ReactNode } & ComponentProps<typeof Button>) {
  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <Button
          type="button"
          size={size}
          variant={variant}
          aria-label={label}
          className={cn('text-muted-foreground hover:text-foreground', className)}
          {...props}
        >
          {children}
        </Button>
      </TooltipTrigger>
      <TooltipContent>{label}</TooltipContent>
    </Tooltip>
  );
}
