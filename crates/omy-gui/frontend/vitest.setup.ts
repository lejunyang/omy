/** 测试全局注册：所有用例共享的组件级配置。
 *
 * 业务组件的模板里直接写 <AppIcon name="..." />。运行期它由 main.ts 全局
 * 注册，测试不走 main.ts——若不在此统一注册，每个 mount 都拿不到该组件，
 * Vue 会报 "Failed to resolve component: AppIcon"，图标位置渲染成空节点。
 *
 * 用 @vue/test-utils 的全局 config 一次性注入，禁止各测试文件逐例
 * 自行 stubs/components 绕过：绕开会让图标在部分用例里是真 SVG、在另一
 * 些里是空壳，截图与交互断言的对象不一致。
 */
import { config } from '@vue/test-utils';
import AppIcon from './src/components/AppIcon.vue';

config.global.components = { AppIcon };
