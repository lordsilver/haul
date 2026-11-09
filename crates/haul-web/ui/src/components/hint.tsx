import type { ComponentProps, ReactElement, ReactNode } from 'react';
import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip';

// What a control does or why it is as it is, as a tooltip that shows on
// hover and on keyboard focus alike. Native `title` is kept for text cut off
// (`truncate`), where it repeats the whole text. A control that is disabled
// takes no pointer events, so it is wrapped in a span that does (`wrap`).
export function Hint({
  text,
  children,
  side,
  wrap = false,
}: {
  text?: ReactNode;
  children: ReactElement;
  side?: ComponentProps<typeof TooltipContent>['side'];
  wrap?: boolean;
}) {
  if (!text) return children;
  return (
    <Tooltip>
      <TooltipTrigger asChild>
        {wrap ? (
          // biome-ignore lint/a11y/noNoninteractiveTabindex: a disabled control's reason has to be reachable from the keyboard
          <span tabIndex={0} className="inline-flex">
            {children}
          </span>
        ) : (
          children
        )}
      </TooltipTrigger>
      <TooltipContent side={side}>{text}</TooltipContent>
    </Tooltip>
  );
}
