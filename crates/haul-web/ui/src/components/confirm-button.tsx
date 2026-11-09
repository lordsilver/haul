import { type ComponentProps, type ReactNode, type RefObject, useEffect, useRef, useState } from 'react';
import { Button } from '@/components/ui/button';
import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip';
import { report } from '@/lib/api';

type Props = {
  label: string;
  confirm?: string;
  action: () => Promise<unknown>;
  icon?: ReactNode;
  iconOnly?: boolean;
  // Words the button stays as wide as, so the buttons beside it keep their
  // place when its label changes.
  reserve?: string;
  // Set to what a click does, for a key that stands for the button (Delete
  // for Remove), second press included where it asks.
  press?: RefObject<(() => void) | null>;
} & Omit<ComponentProps<typeof Button>, 'onClick' | 'children'>;

// A button that, given `confirm`, wants a second click within a few seconds,
// saying what it asks while it waits; without one a single click acts. An
// icon-only one spells the question out. Gaining or losing `confirm` starts it
// afresh, so a question left open is not asked again later.
export const ConfirmButton = (props: Props) => <Asking key={props.confirm ? 'confirm' : 'plain'} {...props} />;

function Asking({
  label,
  confirm,
  action,
  icon,
  iconOnly = false,
  reserve,
  press,
  variant = 'ghost',
  size,
  ...props
}: Props) {
  const [asking, setAsking] = useState(false);
  const timer = useRef<ReturnType<typeof setTimeout>>(undefined);
  useEffect(() => () => clearTimeout(timer.current), []);
  const click = () => {
    clearTimeout(timer.current);
    if (confirm && !asking) {
      setAsking(true);
      timer.current = setTimeout(() => setAsking(false), 4000);
      return;
    }
    setAsking(false);
    action().catch(report);
  };
  if (press) press.current = click;
  const plain = iconOnly && !asking;
  const button = (
    <Button
      type="button"
      variant={asking ? 'destructive' : variant}
      size={plain ? (size ?? 'icon-sm') : size === 'icon-sm' ? 'sm' : size === 'icon-xs' ? 'xs' : size}
      aria-label={asking ? confirm : label}
      onClick={click}
      className={plain ? 'text-muted-foreground hover:text-foreground' : undefined}
      {...props}
    >
      {icon}
      {plain ? null : reserve ? (
        <span className="grid justify-items-start">
          <span className="[grid-area:1/1]">{asking ? confirm : label}</span>
          <span aria-hidden className="invisible [grid-area:1/1]">
            {reserve}
          </span>
        </span>
      ) : asking ? (
        confirm
      ) : (
        label
      )}
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
