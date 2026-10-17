import { useQuickAskSelection } from "@/hooks/useQuickAskSelection";
import { AppLayout } from "@/components/layout/AppLayout";

export default function App() {
  useQuickAskSelection();
  return <AppLayout />;
}
