/** pagedjs 无官方类型，此处声明渲染舱用到的最小面 */
declare module "pagedjs" {
  export interface PagedFlow {
    total?: number;
    pages?: unknown[];
  }
  export class Previewer {
    preview(content: Element, stylesheets?: unknown[], renderTo?: Element): Promise<PagedFlow>;
  }
}
