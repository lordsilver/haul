import { Eye, EyeOff } from 'lucide-react';
import { type ComponentProps, useState } from 'react';
import { IconButton } from '@/components/icon-button';
import { Input } from '@/components/ui/input';
import { cn } from '@/lib/utils';

// A password field with a button that shows what it holds.
export function SecretInput({ className, ...props }: Omit<ComponentProps<typeof Input>, 'type'>) {
  const [shown, setShown] = useState(false);
  return (
    <span className={cn('relative inline-flex', className)}>
      <Input type={shown ? 'text' : 'password'} autoComplete="off" spellCheck={false} className="pr-9" {...props} />
      <IconButton
        label={shown ? 'Hide' : 'Show'}
        size="icon-xs"
        className="absolute top-1/2 right-1.5 -translate-y-1/2"
        onClick={() => setShown(!shown)}
      >
        {shown ? <EyeOff /> : <Eye />}
      </IconButton>
    </span>
  );
}
