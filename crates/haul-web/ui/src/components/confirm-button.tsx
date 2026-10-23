import { type ComponentProps, type ReactNode, useEffect, useRef, useState } from 'react';
import { Button } from '@/components/ui/button';
import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip';
import { report } from '@/lib/api';

// A button that wants a second click within a few seconds, saying what it
// asks while it waits. An icon-only one spells the question out.
export function ConfirmButton({
  label,
  confirm,
  action,
  icon,
  iconOnly = false,
  variant = 'ghost',
  size,
  ...props
}: {
  label: string;
  confirm: string;
  action: () => Promise<unknown>;
  icon?: ReactNode;
  iconOnly?: boolean;
} & Omit<ComponentProps<typeof Button>, 'onClick' | 'children'>) {
  const [asking, setAsking] = useState(false);
  const timer = useRef<ReturnType<typeof setTimeout>>(undefined);
  useEffect(() => () => clearTimeout(timer.current), []);
  const click = () => {
    clearTimeout(timer.current);
    if (!asking) {
      setAsking(true);
      timer.current = setTimeout(() => setAsking(false), 4000);
      return;
    }
    setAsking(false);
    action().catch(report);
  };
  const plain = iconOnly && !asking;
  const button = (
    <Button
      type="button"
      variant={asking ? 'destructive' : variant}
      size={plain ? (size ?? 'icon-sm') : size === 'icon-sm' ? 'sm' : size}
      aria-label={asking ? confirm : label}
      onClick={click}
      className={plain ? 'text-muted-foreground hover:text-foreground' : undefined}
      {...props}
    >
      {icon}
      {plain ? null : asking ? confirm : label}
    </Button>
  );
  if (!plain) return button;
  return (
    <Tooltip>
      <TooltipTrigger asChild>{button}</TooltipTrigger>
      <TooltipContent>{label}</TooltipContent>
    </Tooltip>
  );
}
